pub mod grid;
pub mod pty;
pub mod status;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use uuid::Uuid;

use crate::claude::launcher::{spawn_claude, spawn_shell, ResumeExtras, SessionStart};
use crate::claude::session::{session_path, SessionTailer};
use crate::pane::pty::ReaderHooks;
use crate::pane::status::ClaudeStatus;
use crate::wake::{OutputStamp, OutputWaker, WakeTx};

pub type PaneId = u64;

/// 画面 y=0 に表示されている行のバッファ絶対 row。
///
/// `abs = total_scrolled_off - scrollback_offset` 。選択 (`Selection`) の
/// 絶対座標系の基準で、`abs行 = viewport_top_abs + screen_y`、
/// `screen_y = abs行 - viewport_top_abs` の相互変換に使う。
/// alternate screen 中は両項とも常に 0 → abs == screen_y。
pub fn viewport_top_abs(screen: &vt100::Screen) -> i64 {
    screen.total_scrolled_off() as i64 - screen.scrollback() as i64
}

pub struct Pane {
    pub id: PaneId,
    pub cwd: PathBuf,
    pub session_id: Uuid,
    pub created_at: Instant,
    /// 子プロセス (claude / shell) の PTY。None = 眠っている: 保留 (Alt+C) に
    /// したタブの Claude を落としてメモリを空けた状態 ([`Pane::sleep`]) か、
    /// 保留のセッションをプロセスなしで開いた状態 ([`Pane::dormant`])。
    /// `parser` (最後の画面) は残るので、開けば最後の画面がそのまま見える。
    /// 起こすのは `App::wake_pane` (同じ id で spawn し直して差し替える)。
    pub pty: Option<pty::PtyHandle>,
    pub parser: Arc<Mutex<vt100::Parser>>,
    pub command: String,
    pub claude_running: bool,
    /// このペインの Claude セッション JSONL の追跡子。イベントループの
    /// 定期 tick からのみ更新し、描画側は読むだけ (描画パスに I/O を
    /// 持ち込まないため)。
    pub session: SessionTailer,
    /// Claude の実行状態 (タブ/サイドバーの色分け用)。session と同じく
    /// 定期 tick (`App::refresh_pane_state`) だけが更新するキャッシュ。
    pub status: ClaudeStatus,
    /// 「今なにをしているか」の表示文字列 (OSC タイトル優先、無ければ
    /// セッション JSONL の最後のユーザー発話)。これも tick 更新キャッシュ。
    pub task: Option<String>,
    /// reader スレッドが最後に出力を parser へ反映した時刻。
    /// `CCNEST_LATENCY_TRACE` の計測専用 (描画・入力の判断には使わない)。
    pub output_stamp: OutputStamp,
    /// reader スレッド → イベントループの「出力あり」通知。ループが
    /// `LoopMsg::Output` を受けたら `disarm` する。
    pub waker: OutputWaker,
    /// 直前に PTY / parser へ適用した (rows, cols)。描画矩形が変わったときだけ
    /// `ResizePseudoConsole` と vt100 `set_size` を呼ぶためのキャッシュ。
    last_size: Option<(u16, u16)>,
    /// 最後に `write` した時刻 (`created_at` からのミリ秒 + 1。0 = まだ無い)。
    /// 「入力を送った直後は、まだスピナーが出ていなくても落とさない」の判定用。
    last_write_ms: AtomicU64,
}

/// プロセスなしで開いた保留のセッションの画面に出す案内。
const DORMANT_HINT: &str =
    "保留中のセッションです (Claude は起動していません)。\r\nAlt+C で保留を外すと、会話の続きから再開します (文字を打っても再開します)。\r\n";

/// 純粋判定: 直前に適用したサイズ `last` と要求 (rows, cols) から、適用すべき
/// 新サイズを返す。0 は 1 に丸める (vt100 / ConPTY とも 0 行 0 列は不正)。
/// 同じなら None (= 何もしない)。
pub fn next_size(last: Option<(u16, u16)>, rows: u16, cols: u16) -> Option<(u16, u16)> {
    let want = (rows.max(1), cols.max(1));
    (last != Some(want)).then_some(want)
}

impl Pane {
    pub fn spawn(
        id: PaneId,
        cwd: &Path,
        session_id: Uuid,
        start: SessionStart,
        wake: WakeTx,
    ) -> Result<Self> {
        Self::spawn_with(id, cwd, session_id, start, &ResumeExtras::default(), wake)
    }

    /// [`Pane::spawn`] に、再開セッション個別の指定 (最初の一言・開始モード) を付けたもの。
    pub fn spawn_with(
        id: PaneId,
        cwd: &Path,
        session_id: Uuid,
        start: SessionStart,
        extras: &ResumeExtras,
        wake: WakeTx,
    ) -> Result<Self> {
        let parser = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 2000)));
        let waker = OutputWaker::new(wake, id);
        let hooks = ReaderHooks {
            stamp: OutputStamp::new(),
            waker: Some(waker.clone()),
        };
        let (pty, command, claude_running) = spawn_claude(
            cwd,
            session_id,
            start,
            extras,
            Arc::clone(&parser),
            hooks.clone(),
        )?;
        // claude が起動できなかった (shell フォールバック) ペインは追跡しない。
        let session = if claude_running {
            SessionTailer::new(session_path(cwd, &session_id.to_string()))
        } else {
            SessionTailer::default()
        };
        Ok(Self {
            id,
            cwd: cwd.to_path_buf(),
            session_id,
            created_at: Instant::now(),
            pty: Some(pty),
            parser,
            command,
            claude_running,
            session,
            status: ClaudeStatus::default(),
            task: None,
            output_stamp: hooks.stamp,
            waker,
            last_size: None,
            last_write_ms: AtomicU64::new(0),
        })
    }

    /// 保留のセッションを、プロセスを起こさずに眠ったまま開く (再起動後の復元で
    /// 保留のタブまで claude を起動してメモリを使わないため)。画面には案内だけを
    /// 出す。起こし方は眠らせたペインと同じ (`App::wake_pane`)。
    pub fn dormant(id: PaneId, cwd: &Path, session_id: Uuid, wake: WakeTx) -> Self {
        let mut parser = vt100::Parser::new(24, 80, 2000);
        parser.process(DORMANT_HINT.as_bytes());
        Self {
            id,
            cwd: cwd.to_path_buf(),
            session_id,
            created_at: Instant::now(),
            pty: None,
            parser: Arc::new(Mutex::new(parser)),
            command: "claude".to_string(),
            claude_running: true,
            session: SessionTailer::default(),
            status: ClaudeStatus::NeedsAttention,
            task: None,
            output_stamp: OutputStamp::new(),
            waker: OutputWaker::new(wake, id),
            last_size: None,
            last_write_ms: AtomicU64::new(0),
        }
    }

    /// 眠っているか (プロセスが無い)。
    pub fn asleep(&self) -> bool {
        self.pty.is_none()
    }

    /// 子プロセスを子孫ごと終了させて眠らせる。最後の画面 (`parser`)・セッション
    /// id・タスク名は残す。呼ぶ前に、止めても失う物が無いことを確かめること
    /// (`App::sleep_held_tab`)。すでに眠っていれば何もしない。
    pub fn sleep(&mut self) {
        if let Some(pty) = self.pty.take() {
            pty.kill();
        }
    }

    /// 最後に `write` してからの時間。まだ一度も書いていなければ None。
    pub fn since_last_write(&self) -> Option<Duration> {
        let at = self.last_write_ms.load(Ordering::Relaxed);
        if at == 0 {
            return None;
        }
        let now = self.created_at.elapsed().as_millis() as u64 + 1;
        Some(Duration::from_millis(now.saturating_sub(at)))
    }

    /// 描画矩形が変わったときだけ PTY と parser のサイズを揃える。かつては
    /// 描画クロージャから毎フレーム無条件に呼ばれ、同サイズでも
    /// `ResizePseudoConsole` (conhost 側は console lock + 再描画トリガ) と
    /// vt100 `set_size` (全行 resize) を 33 回/秒払っていた。適用したら true。
    pub fn resize_if_changed(&mut self, rows: u16, cols: u16) -> bool {
        let Some((r, c)) = next_size(self.last_size, rows, cols) else {
            return false;
        };
        if let Some(pty) = &self.pty {
            pty.resize(r, c);
        }
        if let Ok(mut p) = self.parser.lock() {
            p.set_size(r, c);
        }
        self.last_size = Some((r, c));
        true
    }

    /// 子プロセスへ書く。眠っているペインでは何もしない。
    pub fn write(&self, data: &[u8]) {
        let Some(pty) = &self.pty else {
            return;
        };
        pty.write(data);
        self.last_write_ms.store(
            self.created_at.elapsed().as_millis() as u64 + 1,
            Ordering::Relaxed,
        );
    }

    pub fn terminate(self) {
        if let Some(pty) = &self.pty {
            pty.kill();
        }
    }

    /// 現在走っている子プロセス（claude 等）を kill し、同じペイン枠に
    /// 新しいシェル(cmd.exe / $SHELL)を起動し直す。Ctrl+C 2 連打で呼ばれる。
    ///
    /// 新しい vt100::Parser を割り当てて画面状態をリセットする。旧 parser は
    /// 旧 PTY の reader スレッドが EOF まで書き込むが、誰も参照しないので問題ない。
    pub fn respawn_as_shell(&mut self) -> Result<()> {
        if let Some(pty) = &self.pty {
            pty.kill();
        }
        let new_parser = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 2000)));
        let hooks = ReaderHooks {
            stamp: self.output_stamp.clone(),
            waker: Some(self.waker.clone()),
        };
        let (new_pty, cmd_label) = spawn_shell(&self.cwd, Arc::clone(&new_parser), hooks)?;
        self.pty = Some(new_pty);
        self.parser = new_parser;
        // 新しい PTY (40x140) / parser (24x80) は矩形と食い違うので、次の描画後の
        // sync で必ずリサイズさせる。
        self.last_size = None;
        self.command = cmd_label;
        self.claude_running = false;
        // claude は死んだのでセッション追跡も畳む。これを忘れるとコンパイルは
        // 通ったまま、shell に戻ったペインに古いモデル名が出続ける。
        self.session.disable();
        // 状態色とタスク名も claude のものなので必ず捨てる。残すと shell に
        // 戻ったペインに古いタスク名・状態色が表示され続ける。
        self.status = ClaudeStatus::Idle;
        self.task = None;
        Ok(())
    }

    /// スクロールバック位置を delta 行ぶん移動する（+ で履歴へ、- で現在へ）。
    /// vt100 側でクランプされるので範囲チェック不要。
    pub fn scroll_by(&self, delta: i32) {
        let Ok(mut p) = self.parser.lock() else {
            return;
        };
        let cur = p.screen().scrollback() as i32;
        let next = (cur + delta).max(0) as usize;
        p.set_scrollback(next);
    }

    /// 履歴表示を解除して最新行へ戻る。
    pub fn scroll_to_bottom(&self) {
        if let Ok(mut p) = self.parser.lock() {
            p.set_scrollback(0);
        }
    }

    /// 現在のビューポート先頭 (画面 y=0) のバッファ絶対 row。
    /// ロック失敗時は 0 を返す（呼び出し側は差分 0 = no-op として扱える）。
    pub fn top_abs(&self) -> i64 {
        self.parser
            .lock()
            .ok()
            .map(|p| viewport_top_abs(p.screen()))
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::next_size;

    #[test]
    fn next_size_is_none_when_unchanged() {
        assert_eq!(next_size(Some((40, 140)), 40, 140), None);
    }

    #[test]
    fn next_size_applies_first_time_and_on_change() {
        assert_eq!(next_size(None, 40, 140), Some((40, 140)));
        assert_eq!(next_size(Some((40, 140)), 41, 140), Some((41, 140)));
        assert_eq!(next_size(Some((40, 140)), 40, 139), Some((40, 139)));
    }

    #[test]
    fn next_size_clamps_zero_to_one_and_dedups_after_clamp() {
        assert_eq!(next_size(None, 0, 0), Some((1, 1)));
        assert_eq!(next_size(Some((1, 1)), 0, 0), None);
    }
}

// Version History
// ver0.5 - 2026-11-21 - DORMANT_HINT names the key that wakes the pane (Alt+C;
//                       typing into the pane also wakes it).
// ver0.4 - 2026-11-21 - A pane can be asleep: `pty` is an Option, None = no child
//                       process. sleep() kills the process tree and keeps the
//                       last screen (parser), session id and task name;
//                       dormant() opens a held session without starting claude
//                       (a hint on the screen). write() is a no-op while asleep
//                       and records the time of the last write
//                       (since_last_write) so a pane that was just sent input
//                       is not put to sleep.
// ver0.3 - 2026-11-21 - Pane::spawn_with: spawn with ResumeExtras (first prompt /
//                       per-session permission mode); Pane::spawn delegates.
// ver0.2 - 2026-10-19 - Pane::spawn takes a SessionStart so a pane can resume an
//                       existing Claude session (`--resume <id>`).
// ver0.1 - 2026-09-06 - Pane owns an OutputWaker (reader → event loop
//                       notification) and caches last_size so
//                       resize_if_changed() only touches the PTY / parser when
//                       the drawn rect actually changed (pure next_size()).
//                       respawn_as_shell resets the cache.
