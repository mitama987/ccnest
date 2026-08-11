pub mod grid;
pub mod pty;
pub mod status;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use uuid::Uuid;

use crate::claude::launcher::{spawn_claude, spawn_shell};
use crate::claude::session::{session_path, SessionTailer};
use crate::pane::status::ClaudeStatus;

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
    pub pty: pty::PtyHandle,
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
}

impl Pane {
    pub fn spawn(id: PaneId, cwd: &Path, session_id: Uuid) -> Result<Self> {
        let parser = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 2000)));
        let (pty, command, claude_running) = spawn_claude(cwd, session_id, Arc::clone(&parser))?;
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
            pty,
            parser,
            command,
            claude_running,
            session,
            status: ClaudeStatus::default(),
            task: None,
        })
    }

    pub fn resize(&self, rows: u16, cols: u16) {
        self.pty.resize(rows, cols);
        if let Ok(mut p) = self.parser.lock() {
            p.set_size(rows, cols);
        }
    }

    pub fn write(&self, data: &[u8]) {
        self.pty.write(data);
    }

    pub fn terminate(self) {
        self.pty.kill();
    }

    /// 現在走っている子プロセス（claude 等）を kill し、同じペイン枠に
    /// 新しいシェル(cmd.exe / $SHELL)を起動し直す。Ctrl+C 2 連打で呼ばれる。
    ///
    /// 新しい vt100::Parser を割り当てて画面状態をリセットする。旧 parser は
    /// 旧 PTY の reader スレッドが EOF まで書き込むが、誰も参照しないので問題ない。
    pub fn respawn_as_shell(&mut self) -> Result<()> {
        self.pty.kill();
        let new_parser = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 2000)));
        let (new_pty, cmd_label) = spawn_shell(&self.cwd, Arc::clone(&new_parser))?;
        self.pty = new_pty;
        self.parser = new_parser;
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
