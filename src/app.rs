use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use uuid::Uuid;

use crate::claude::jp_title::{pick_task, JpTitler};
use crate::claude::launcher::{ResumeExtras, SessionStart};
use crate::claude::usage::{UsageCell, UsageLabel, UsageSnapshot};
use crate::cli::ResumeSpec;
use crate::pane::grid::{Direction, Layout};
use crate::pane::status::{
    aggregate_status, detect_status, is_safe_to_stop, sanitize_task, screen_text, ClaudeStatus,
};
use crate::pane::{Pane, PaneId};
use crate::remote::{self, Inbox, Reply};
use crate::sidebar::SidebarState;
use crate::wake::{WakeRx, WakeTx};

pub struct Tab {
    pub title: String,
    /// 手で付けた名前 (タブをダブルクリック / Alt+F)。あれば Claude のタスク名
    /// (`/rename`・日本語名・端末タイトル・最後の発話) より優先して `N:名前` で出す。
    /// `tab_names` にセッション id ごとに保存する。
    pub user_title: Option<String>,
    /// 緑 (実行中／裏で実行中) が続いているので、タブバーの右側に小さく
    /// (`🟢番号` だけで) 寄せてある。対応が必要 (🟨) になったら左へ戻る。
    /// 2 秒 tick の [`App::refresh_tab_parking`] だけが変える (クリックの瞬間に
    /// タブの位置を動かさないため)。
    pub parked: bool,
    /// 緑が続いている起点。[`PARK_AFTER`] 続いたら `parked` にする。
    pub working_since: Option<Instant>,
    /// 保留 (Alt+C)。タブバーのいちばん右に、灰色のマーカー + 番号だけで出す。
    /// 状態 (🟢／🟨) が変わっても、もう一度 Alt+C を押すまで保留のまま。
    /// `tab_holds` にセッション id ごとに保存する。`parked` は裏で更新し続ける
    /// (保留を外した瞬間に正しい組へ入るように)。
    pub held: bool,
    pub layout: Layout,
    pub focused: PaneId,
    /// タブバーでの「すべり」の途中 (ドラッグで押しのけられた／離されて収まる)。
    /// 描画は定位置からこのずれだけ離して描く。終わったら None。
    pub slide: Option<TabSlide>,
}

impl Tab {
    /// タブバーでの組 ([`tab_group`])。
    pub fn group(&self) -> TabGroup {
        tab_group(self.held, self.parked)
    }
}

/// タブバーでの組。並べる順でもある (左 → 右)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TabGroup {
    /// 名前つきで左に並ぶ (対応が必要 🟨・緑になったばかり・shell だけのタブ)。
    Left,
    /// 緑が続いていて、右へ小さく寄せてある (`🟢番号`)。
    Working,
    /// 保留。いちばん右に、灰色のマーカー + 番号だけ。
    Held,
}

/// 純粋関数: タブの組。保留は状態より優先 (緑が続いていても保留の組)。
pub fn tab_group(held: bool, parked: bool) -> TabGroup {
    if held {
        TabGroup::Held
    } else if parked {
        TabGroup::Working
    } else {
        TabGroup::Left
    }
}

/// タブがすべる時間。
pub const TAB_SLIDE: Duration = Duration::from_millis(140);

/// タブバーでのすべり 1 回ぶん。`from_offset` = 始まったときの「定位置からのずれ」
/// (桁。+ は右)、`start` から `TAB_SLIDE` かけて 0 へ近づく。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabSlide {
    pub from_offset: i32,
    pub start: Instant,
}

impl TabSlide {
    /// `now` 時点の定位置からのずれ。
    pub fn offset_at(&self, now: Instant) -> i32 {
        slide_offset(
            self.from_offset,
            now.saturating_duration_since(self.start),
            TAB_SLIDE,
        )
    }

    pub fn finished(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.start) >= TAB_SLIDE
    }
}

/// タブバーで左ボタンを押して引いている最中の状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabDrag {
    /// 引いているタブの「今の」位置 (`move_tab` のたびに更新)。
    pub idx: usize,
    /// 押したときのポインタの桁。ここから動いたら浮かせる。
    pub press_x: i32,
    /// 押した桁 − タブの左端。浮いたタブをポインタの下の同じ所に保つ。
    pub grab_dx: i32,
    /// いまのポインタの桁。
    pub pointer_x: i32,
    /// 浮いているか (押した桁から動いた)。クリックだけなら false のまま。
    pub floating: bool,
}

/// `CCNEST_TAB_ANIM=off` (または `0`) でタブのアニメーションを止める
/// (浮かせない・すべらせない = v0.1.37 と同じ即時の並べ替え)。初回だけ評価。
pub fn tab_anim_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        !std::env::var("CCNEST_TAB_ANIM").is_ok_and(|v| v.eq_ignore_ascii_case("off") || v == "0")
    })
}

/// 緑になってから右へ寄せるまでの猶予。すぐ終わるターン (緑 → すぐ 🟨) で
/// タブが右へ行って戻るのを避ける。🟨 に変わったときは待たずに左へ戻す。
pub const PARK_AFTER: Duration = Duration::from_secs(4);

/// タブをクリックした直後は並びを変えない時間 (ダブルクリックの 2 回目が
/// 別のタブに当たらないように。ダブルクリックの間隔 500ms より少し長く)。
const PARK_FREEZE_AFTER_CLICK: Duration = Duration::from_millis(600);

/// `CCNEST_TAB_GROUP=off` (または `0`) で「緑のタブを右へ小さく寄せる」を止める
/// (v0.1.45 までと同じ、並び順どおり・全部名前つき)。初回だけ評価。
pub fn tab_group_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        !std::env::var("CCNEST_TAB_GROUP").is_ok_and(|v| v.eq_ignore_ascii_case("off") || v == "0")
    })
}

/// 純粋関数: 1 tick ぶんの「右へ寄せるか」。`prev` = 緑が続いている起点
/// (`Tab::working_since`)、`working` = いま緑か。戻り値は (新しい起点, 寄せるか)。
/// 緑でなくなったら起点ごと消す (= すぐ左へ戻る)。緑が [`PARK_AFTER`] 続いたら寄せる。
pub fn park_step(prev: Option<Instant>, working: bool, now: Instant) -> (Option<Instant>, bool) {
    if !working {
        return (None, false);
    }
    let since = prev.unwrap_or(now);
    (
        Some(since),
        now.saturating_duration_since(since) >= PARK_AFTER,
    )
}

/// 入力を送ってからこの時間は、画面が「止めてよい」形でも Claude を落とさない。
/// 送った直後は、まだスピナー行が画面に出ていないことがある。
pub const SLEEP_QUIET_AFTER_INPUT: Duration = Duration::from_secs(5);

/// お知らせ (ステータスバーの左) を出しておく時間。
pub const NOTICE_TTL: Duration = Duration::from_secs(8);

/// `CCNEST_HOLD_SLEEP=off` (または `0`) で「保留にしたタブの Claude を落として
/// メモリを空ける」を止める (v0.1.48 と同じ、保留は見た目だけ)。初回だけ評価。
pub fn hold_sleep_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        !std::env::var("CCNEST_HOLD_SLEEP").is_ok_and(|v| v.eq_ignore_ascii_case("off") || v == "0")
    })
}

/// 保留にしたタブのペイン 1 つを眠らせる (Claude を落とす) かの判定結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepCheck {
    /// 落としてよい。
    Stop,
    /// claude のペインではない (shell) か、もう眠っている。数えない。
    Skip,
    /// 落とさない: 実行中・裏の仕事あり・ダイアログの途中・書きかけの入力あり・
    /// 入力を送った直後・画面を読めなかった、のどれか。
    Keep,
}

/// 純粋関数: ペインを眠らせるか。`safe_screen` = いまの画面が「止めても失う物が
/// 無い」形か ([`is_safe_to_stop`])。画面を読めなかったら None。`since_write` =
/// 最後に入力を送ってからの時間。「落としてよい」と言えるときだけ [`SleepCheck::Stop`]。
pub fn sleep_check(
    claude_running: bool,
    asleep: bool,
    safe_screen: Option<bool>,
    since_write: Option<Duration>,
) -> SleepCheck {
    if !claude_running || asleep {
        return SleepCheck::Skip;
    }
    if since_write.is_some_and(|d| d < SLEEP_QUIET_AFTER_INPUT) {
        return SleepCheck::Keep;
    }
    match safe_screen {
        Some(true) => SleepCheck::Stop,
        _ => SleepCheck::Keep,
    }
}

/// 純粋関数: 保留にしたときのお知らせ。`stopped` = 落としたペインの数、`kept` =
/// 落とさなかった claude のペインの数。どちらも 0 (shell だけのタブ) なら None。
pub fn hold_notice(stopped: usize, kept: usize) -> Option<String> {
    match (stopped, kept) {
        (0, 0) => None,
        (_, 0) => Some(
            "保留: Claude を止めてメモリを空けました (開いてキーを押すと続きから再開)".to_string(),
        ),
        (0, _) => Some("保留: 作業中か入力の途中なので Claude は動かしたままです".to_string()),
        (s, k) => Some(format!(
            "保留: Claude を {s} 個止めました ({k} 個は作業中か入力の途中なのでそのまま)"
        )),
    }
}

/// 純粋関数: タブバーに並べる順 (タブ番号の列)。組の順 (名前つき → 緑で小さく
/// 寄せた → 保留) に並べ、どの組の中でも元の順は変えない (🟨 に戻ったタブ・
/// 保留を外したタブは、元いた並びの位置に戻る)。
pub fn display_order(groups: &[TabGroup]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..groups.len()).collect();
    // sort_by_key は安定 = 同じ組の中の順は変わらない。
    order.sort_by_key(|i| groups[*i]);
    order
}

/// 純粋関数: タブ `idx` を保留にしたあと前面に出すタブ (メールのアーカイブと
/// 同じで、保留にしたら次へ進む)。`order`・`groups` は保留にする前のタブバーの
/// 並びと組。同じ組の右隣 → 同じ組の左隣 → 保留でない右のタブ → 保留でない
/// 左のタブ。どれも無ければ (ほかが全部保留) そのまま。
pub fn active_after_hold(order: &[usize], groups: &[TabGroup], idx: usize) -> usize {
    let Some(pos) = order.iter().position(|i| *i == idx) else {
        return idx;
    };
    let Some(own) = groups.get(idx).copied() else {
        return idx;
    };
    let group_of = |i: &usize| groups.get(*i).copied();
    let right = &order[pos + 1..];
    let left = &order[..pos];
    let same = |i: &&usize| group_of(i) == Some(own);
    let open = |i: &&usize| group_of(i).is_some_and(|g| g != TabGroup::Held);
    right
        .iter()
        .find(same)
        .or_else(|| left.iter().rev().find(same))
        .or_else(|| right.iter().find(open))
        .or_else(|| left.iter().rev().find(open))
        .copied()
        .unwrap_or(idx)
}

/// 純粋関数: `order` (タブバーの並び) の上で、`active` の隣 (`dir` = +1 右 / -1 左)。
/// 端は反対側へ回る。`active` が `order` に無ければ動かさない。
pub fn neighbor_in_order(order: &[usize], active: usize, dir: i32) -> usize {
    let Some(pos) = order.iter().position(|i| *i == active) else {
        return active;
    };
    let n = order.len() as i32;
    order[(pos as i32 + dir).rem_euclid(n) as usize]
}

/// 閉じたタブの控え (Ctrl+Shift+T で開き直す用)。
pub struct ClosedTab {
    pub title: String,
    /// 手で付けた名前 (開き直したタブに戻す)。
    pub user_title: Option<String>,
    /// 保留だったか (開き直したタブも保留のまま)。
    pub held: bool,
    /// 閉じる前のタブの位置。
    pub index: usize,
    /// 閉じる前のペイン ID のままの分割の形。
    pub layout: Layout,
    pub focused: PaneId,
    pub panes: Vec<ClosedPane>,
}

pub struct ClosedPane {
    pub old_id: PaneId,
    pub cwd: PathBuf,
    pub session_id: Uuid,
    /// Claude の会話ログ (`~/.claude/projects/<enc>/<id>.jsonl`) がある = `--resume` できる。
    pub resumable: bool,
}

/// 覚えておく閉じたタブの数。
const CLOSED_TABS_MAX: usize = 20;

pub struct App {
    pub cwd: PathBuf,
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    pub panes: HashMap<PaneId, Pane>,
    pub next_pane_id: PaneId,
    pub sidebar: SidebarState,
    pub sidebar_focused: bool,
    pub quit: bool,
    pub status: Option<String>,
    /// 一時的なお知らせ (文面, 出した時刻)。ステータスバーの左に [`NOTICE_TTL`] の
    /// あいだ出す (`status` と違って時間で消える)。
    pub notice: Option<(String, Instant)>,
    pub renaming_tab: Option<String>,
    /// 名前の入力を始めてから、まだ何も打っていない (今の名前が丸ごと選ばれて
    /// いる状態。Explorer の名前変更と同じ)。最初の 1 文字・貼り付けは丸ごと
    /// 置き換え、Backspace は全部消す。そのまま Enter なら何も変えない。
    pub rename_fresh: bool,
    /// 直近にタブを左クリックした (時刻, タブ番号)。500ms 以内に同じタブを
    /// もう一度押したらダブルクリック = 名前の入力を始める。タブの並びが
    /// 変わったら (並べ替え・閉じる・開き直し) 捨てる。
    pub last_tab_click: Option<(Instant, usize)>,
    /// 直近の Ctrl+C の (ペイン, 押下時刻)。同ペインで閾値内に再度 Ctrl+C が
    /// 来たら claude→shell の再起動をトリガする。
    pub last_ctrl_c: Option<(PaneId, Instant)>,
    /// マウスドラッグで作られた画面テキスト選択範囲。選択がある状態で Ctrl+C を
    /// 押すとクリップボードへコピーされる。
    pub selection: Option<Selection>,
    /// 直近の左クリックの (押下時刻, ペイン, ペイン内 row)。閾値内に同じペインの
    /// 同じ行で再クリックされたらダブルクリックと判定し、その行を全選択する。
    pub last_left_click: Option<(Instant, PaneId, u16)>,
    /// 直近に修飾キーなしで左クリックした位置。選択が無いときに Alt / Shift +
    /// 左クリックすると、ここからクリック位置までを選ぶ (ブラウザの Shift+クリック)。
    pub click_anchor: Option<ClickAnchor>,
    /// 直近のマウスホイールイベント時刻。Windows ConPTY がホイール 1 回転を
    /// Mouse(ScrollUp/Down) と plain な Up/Down KeyEvent の両方として
    /// (順不同・別バッチで) 配信するため、後者「ファントム矢印」を相殺する
    /// 突き合わせに使う。process_batch でホイールを見たら更新する。
    pub last_wheel_at: Option<Instant>,
    /// 非隣接で届いた plain Up/Down の保留。直後に対のホイールが来れば
    /// ファントムとして破棄し、`PAIR_WINDOW` を超えても来なければ実キーとして
    /// 子へフラッシュする。これによりホイールとファントムが別バッチ・逆順で
    /// 来てもジェスチャ端で取りこぼさない (決定論的コアレス)。
    pub pending_arrow: Option<(crossterm::event::KeyEvent, Instant)>,
    /// マウスモード ON のペインで左ボタンを押した直後の保留状態。
    /// 「クリック (= アプリへ転送)」か「ドラッグ (= ccnest ローカル選択)」かを
    /// 次イベントで判定するまで Down を転送せずここに溜める。
    pub pending_mouse: Option<PendingMouse>,
    /// マウスモード ON のペインで素ドラッグによる ccnest ローカル選択が進行中。
    /// この間 Drag/Up は転送せず既存ローカル選択アームへ流す。
    /// もう 1 つの用途: コンテキストメニューが Down(Left) を消費した直後に立てる
    /// ことで、対の Up(Left) が press なしの release としてマウスモードの子へ
    /// 転送されるのを防ぐ (Up はローカルの無害なアームで消化される)。
    pub mouse_local_drag: bool,
    /// 右クリックで開くコンテキストメニュー。開いている間はキー/マウス入力を
    /// モーダルに横取りする (renaming_tab と同じ Option モーダルパターン)。
    pub context_menu: Option<ContextMenu>,
    /// タブバーで左ボタンを押して引いている最中の状態。引くと `move_tab` で
    /// タブがついてくるので、動かすたびに `idx` も更新する。
    /// ボタンを離すか、別の操作でタブの並びが変わったら None。
    pub tab_drag: Option<TabDrag>,
    /// cwd -> (ブランチ名, 最終探索時刻)。複数ペインが同じ cwd を共有しがちなので、
    /// リポジトリ探索の重複を避けるため App 側に持つ。`refresh_pane_state` が
    /// 現存ペインの cwd だけで作り直すので、ペインを閉じてもエントリが残らない。
    /// 探索は `BRANCH_TTL` ごと (毎 tick ではない)。
    pub branch_cache: BranchCache,
    /// `CCNEST_LATENCY_TRACE` 用: 直近にフォーカスペインへキーを書き込んだ時刻
    /// (`wake::now_us`)。エコーが描画されたら None に戻す。無効時は常に None。
    pub last_key_write_us: Option<u64>,
    /// `CCNEST_LATENCY_TRACE` 用: 直近バッチのペースト判定待ち (us)。
    pub last_burst_wait_us: u64,
    /// 直近にフォーカスペインへキー/ペーストを書いた時刻。書き込み直後に届く
    /// 最初の PTY 出力 (= エコー) は出力フレーム cap を待たずに描くための印。
    /// ストリーミング中は cap が常に温まっているので、これが無いとエコーだけ
    /// 0〜8ms (タイマー分解能込みで最大 16ms) 余計に遅れる。
    pub echo_pending: Option<Instant>,
    /// 直近に子へ転送したマウス移動 (pane, col, row)。同じセル内の Moved は
    /// 子へ再送しない (AnyMotion モードの子にポインタ移動の洪水を流さない)。
    pub last_forwarded_move: Option<(PaneId, u16, u16)>,
    /// 利用制限ポーラー (`claude::usage::spawn_usage_poller`) が最新値を置く所。
    pub usage_cell: UsageCell,
    /// 描画用の利用制限。`refresh_pane_state` が `usage_cell` からコピーする
    /// (描画パスではロックを取らない)。
    pub usage: Option<UsageSnapshot>,
    /// 英語の ai-title を日本語の短いタブ名にする係 (`claude::jp_title`)。
    pub jp_titler: JpTitler,
    /// `--resume` で渡され、まだタブを開いていないセッション。
    pub pending_resumes: VecDeque<ResumeSpec>,
    /// 閉じたタブ (新しいものが末尾)。Ctrl+Shift+T で末尾から開き直す。
    pub closed_tabs: Vec<ClosedTab>,
    /// `ccnest --add` の依頼が届くフォルダ (`remote::Inbox`)。作れなかったら
    /// None (依頼を受けないだけ)。App と一緒に drop されてフォルダが消える。
    inbox: Option<Inbox>,
    /// イベントループを起こすチャネルの送信側。ペイン生成時に reader へ配る。
    wake_tx: WakeTx,
    /// 受信側。`run_event_loop` が起動時に `take_wake_rx` で持っていく。
    wake_rx: Option<WakeRx>,
}

/// cwd -> (ブランチ名, 最終探索時刻)。`plan_branch_refresh` と共用する型。
pub type BranchCache = HashMap<PathBuf, (Option<String>, Instant)>;

/// git ブランチ再探索の間隔。`Repository::discover` は親ディレクトリ walk を伴い
/// イベントループ tick には重いため、TTL を切って引き直す。外部での `git switch`
/// が表示に反映されるまで最大 TTL+tick 遅れるが、ステータスバー表示専用なので許容。
const BRANCH_TTL: Duration = Duration::from_secs(10);

/// ペインの claude を起動してから日本語タブ名の生成を頼むまでの猶予。
/// 起動直後の claude と `claude -p` が同時に `~/.claude.json` を書かないように。
const JP_TITLE_REQUEST_AFTER: Duration = Duration::from_secs(10);

/// 左 Down 押下の保留 (press/drag 判定待ち)。座標はペインローカル (0 始まり)。
#[derive(Debug, Clone, Copy)]
pub struct PendingMouse {
    pub pid: PaneId,
    pub lx: u16,
    pub ly: u16,
    pub at: Instant,
}

/// ペイン内のテキスト選択範囲。anchor/cursor は (col, バッファ絶対 row)。
///
/// row はバッファ絶対座標:
/// `abs = viewport_top_abs + screen_y`（`viewport_top_abs = total_scrolled_off - scrollback_offset`）。
/// コンテンツがスクロールバックへ流れても・ホイールで表示位置が変わっても、
/// 同じ内容行は同じ abs を保つため、選択は構造的に内容へ張り付く（描画時に
/// 現在の viewport_top を引いて画面座標へ変換する）。表示範囲より上の行は
/// 変換結果が負になるだけで、値そのものは常に非負とは限らない i64 で持つ。
///
/// 例外はドラッグ中の cursor のみ: マウス直下に留まる必要があるため、
/// ビューポートが動いたら明示的に補正する（scroll_pane_with_selection /
/// advance_drag_auto_scroll）。マウス静止中にストリーミング出力で内容が
/// 流れた場合の cursor は内容側へ張り付いたままとなり、次の Drag イベントで
/// 自然に補正される（意図的なトレードオフ）。
#[derive(Debug, Clone, Copy)]
pub struct Selection {
    pub pane_id: PaneId,
    pub anchor: (u16, i64),
    pub cursor: (u16, i64),
    /// マウスボタン押下中。false になっても選択は残し続け、Ctrl+C か
    /// 新規クリックでクリアされる。
    pub dragging: bool,
    /// drag 中にペイン端を超えたときの auto-scroll 方向。
    /// `-1` = 上端外 (古い行へ), `+1` = 下端外 (新しい行へ), `0` = ペイン内。
    /// イベントループの tick で参照され、1 行/tick で scroll_by に反映される。
    pub auto_scroll: i32,
    /// 選択作成時に子アプリが alternate screen だったか。現在の状態と食い違ったら
    /// (グリッドが入れ替わったら) その選択は無効 → validate_selection が破棄する。
    pub alt: bool,
    /// 選択作成時の vt100 reset 世代 (RIS = ESC c で増える)。RIS はグリッドと
    /// total_scrolled_off を作り直し abs 座標の連続性が切れるため、世代が
    /// 食い違った選択は無効 → validate_selection / 抽出ガードが破棄する。
    pub grid_gen: u64,
}

/// 修飾キーなしの左クリックで控える「選択を伸ばすときの始点」。
///
/// `pos` は `Selection` と同じ (col, バッファ絶対 row) なので、クリックのあとに
/// ホイールで履歴を動かしても同じ内容を指し続ける。指す内容が変わり得る場面
/// では使わない: 別のペイン、alternate screen の出入り (`alt`)、RIS
/// (`grid_gen`)、ペインの大きさの変化 (`size` = クリック時の (幅, 高さ)。
/// 幅が変わると行の折り返しが変わる)。alt 画面の子へホイールを流したときは
/// 子が中身を動かすので、その場で捨てる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClickAnchor {
    pub pane_id: PaneId,
    pub pos: (u16, i64),
    pub alt: bool,
    pub grid_gen: u64,
    pub size: (i32, i32),
}

/// 右クリックで開くコンテキストメニュー。
#[derive(Debug, Clone)]
pub struct ContextMenu {
    /// 右クリックされたペイン。コピー/貼り付けはこのペインを対象にする
    /// (フォーカス中ペインではない点に注意)。
    pub pane_id: PaneId,
    /// 右クリックされたスクリーンセル (端末絶対座標)。描画時にここを基準に
    /// 端末内へクランプしたメニュー矩形を組み立てる。
    pub anchor: (u16, u16),
    pub items: Vec<MenuItem>,
    /// ハイライト中の項目 index (items への添字)。
    pub highlighted: usize,
}

#[derive(Debug, Clone)]
pub struct MenuItem {
    pub label: &'static str,
    pub action: MenuAction,
    /// false = 淡色表示・実行不可 (例: 選択が無いペインでのコピー)。
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    Copy,
    Paste,
}

/// タブに出す名前 (番号・マーカー抜き): 手で付けた名前があればそれ、無ければ
/// フォーカスペインのタスク名。どちらも無ければ None (呼び出し側がタブの
/// タイトルを出す)。描画 (`ui::draw_tabbar`) と名前の入力の初期値で共用。
pub fn shown_tab_name<'a>(user_title: Option<&'a str>, task: Option<&'a str>) -> Option<&'a str> {
    user_title.or(task)
}

/// セッション `id` に手で付けて保存してある名前 (`tab_names`)。
fn saved_tab_name(session_id: &Uuid) -> Option<String> {
    let path = crate::tab_names::store_path()?;
    crate::tab_names::lookup(&path, &session_id.to_string())
}

/// セッション `id` が保留として保存してあるか (`tab_holds`)。
fn saved_tab_hold(session_id: &Uuid) -> bool {
    crate::tab_holds::store_path()
        .is_some_and(|path| crate::tab_holds::is_held(&path, &session_id.to_string()))
}

/// Derive a tab title from the pane cwd (final path component).
pub fn folder_title(cwd: &Path) -> String {
    cwd.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| cwd.display().to_string())
}

impl App {
    pub fn new(cwd: PathBuf) -> Result<Self> {
        Self::with_resumes(cwd, Vec::new())
    }

    /// `resumes` が空なら [`App::new`] と同じ (新規 1 ペイン)。あれば先頭の
    /// セッションで最初のタブを開き、残りは `pending_resumes` に積んで
    /// 2 秒 tick ごとに 1 タブずつ開く ([`App::spawn_next_resume`])。
    pub fn with_resumes(cwd: PathBuf, resumes: Vec<ResumeSpec>) -> Result<Self> {
        let mut pending_resumes: VecDeque<ResumeSpec> = resumes.into();
        let (session_id, start, title, extras) = match pending_resumes.pop_front() {
            Some(r) => {
                let extras = resume_extras(&r);
                (r.session_id, SessionStart::Resume, r.title, extras)
            }
            None => (
                Uuid::new_v4(),
                SessionStart::New,
                None,
                ResumeExtras::default(),
            ),
        };
        let mut panes = HashMap::new();
        let first_id = 1 as PaneId;
        let (wake_tx, wake_rx) = std::sync::mpsc::channel();
        let first = Pane::spawn_with(first_id, &cwd, session_id, start, &extras, wake_tx.clone())?;
        panes.insert(first_id, first);

        let tab = Tab {
            title: title.unwrap_or_else(|| folder_title(&cwd)),
            user_title: saved_tab_name(&session_id),
            parked: false,
            working_since: None,
            held: saved_tab_hold(&session_id),
            layout: Layout::Leaf(first_id),
            focused: first_id,
            slide: None,
        };

        Ok(Self {
            cwd: cwd.clone(),
            tabs: vec![tab],
            active_tab: 0,
            panes,
            next_pane_id: first_id + 1,
            sidebar: SidebarState::new(cwd),
            sidebar_focused: false,
            quit: false,
            status: None,
            notice: None,
            renaming_tab: None,
            rename_fresh: false,
            last_tab_click: None,
            last_ctrl_c: None,
            selection: None,
            last_left_click: None,
            click_anchor: None,
            last_wheel_at: None,
            pending_arrow: None,
            pending_mouse: None,
            mouse_local_drag: false,
            context_menu: None,
            tab_drag: None,
            branch_cache: HashMap::new(),
            last_key_write_us: None,
            last_burst_wait_us: 0,
            echo_pending: None,
            last_forwarded_move: None,
            usage_cell: UsageCell::default(),
            usage: None,
            jp_titler: JpTitler::from_env(),
            pending_resumes,
            closed_tabs: Vec::new(),
            inbox: Inbox::open_own(),
            wake_tx,
            wake_rx: Some(wake_rx),
        })
    }

    /// 開き待ちの再開セッションを 1 つだけ新しいタブで開く。アクティブタブは
    /// 変えない (開いている間に操作中のタブを奪わない)。開いたら true。
    /// 一度に全部 spawn しないのは、並行起動した claude が `~/.claude.json` を
    /// 同時に書いて壊すのを避けるため。
    pub fn spawn_next_resume(&mut self) -> Result<bool> {
        let Some(spec) = self.pending_resumes.pop_front() else {
            return Ok(false);
        };
        let cwd = self.cwd.clone();
        let new_id = self.next_pane_id;
        self.next_pane_id += 1;
        let held = saved_tab_hold(&spec.session_id);
        // 保留のセッションは claude を起こさず、眠ったまま開く (再起動後の復元で
        // 保留のタブにまでメモリを使わない)。最初の一言がある (続きを頼まれて
        // いる) セッションは、保留でも起こす。
        let pane = if opens_dormant(held, spec.prompt.is_some(), hold_sleep_enabled()) {
            Pane::dormant(new_id, &cwd, spec.session_id, self.wake_tx.clone())
        } else {
            Pane::spawn_with(
                new_id,
                &cwd,
                spec.session_id,
                SessionStart::Resume,
                &resume_extras(&spec),
                self.wake_tx.clone(),
            )?
        };
        self.panes.insert(new_id, pane);
        self.tabs.push(Tab {
            title: spec.title.unwrap_or_else(|| folder_title(&cwd)),
            user_title: saved_tab_name(&spec.session_id),
            parked: false,
            working_since: None,
            held,
            layout: Layout::Leaf(new_id),
            focused: new_id,
            slide: None,
        });
        Ok(true)
    }

    /// `ccnest --add` で届いた依頼を拾い、開けるセッションを `pending_resumes`
    /// に積む (実際に開くのは [`App::spawn_next_resume`] が 1 tick 1 タブ)。
    /// 結果は依頼ごとに `.done` で返す。1 件でも受け取ったら true。
    /// イベントループの 2 秒 tick からのみ呼ぶ。
    pub fn accept_remote_requests(&mut self) -> bool {
        let Some(dir) = self.inbox.as_ref().map(|i| i.dir().to_path_buf()) else {
            return false;
        };
        let incoming = remote::take_requests(&dir);
        if incoming.is_empty() {
            return false;
        }
        let mut added = 0;
        for inc in incoming {
            let reply = match inc.request {
                Ok(req) => {
                    // シェルに切り戻したペインのセッションは「開いている」に数えない。
                    let open: HashSet<Uuid> = self
                        .panes
                        .values()
                        .filter(|p| p.claude_running)
                        .map(|p| p.session_id)
                        .chain(self.pending_resumes.iter().map(|r| r.session_id))
                        .collect();
                    let cwd = &self.cwd;
                    let (accepted, skipped) = remote::plan_requests(req.resumes, &open, |id| {
                        crate::claude::session::session_path(cwd, &id.to_string())
                            .is_some_and(|path| path.is_file())
                    });
                    added += accepted.len();
                    let reply = Reply {
                        accepted: accepted.iter().map(|r| r.session_id).collect(),
                        skipped,
                        error: None,
                    };
                    self.pending_resumes.extend(accepted);
                    reply
                }
                Err(e) => Reply {
                    error: Some(e),
                    ..Reply::default()
                },
            };
            // 返事を書けなくても、積んだタブは開く (送る側が時間切れになるだけ)。
            let _ = remote::write_reply(&inc.reply_path, &reply);
        }
        if added > 0 {
            self.status = Some(format!("タブを追加: {added} 件"));
        }
        true
    }

    /// 入力ポンプスレッドなどに配る送信側。
    pub fn wake_tx(&self) -> WakeTx {
        self.wake_tx.clone()
    }

    /// 受信側を取り出す (1 回だけ)。
    pub fn take_wake_rx(&mut self) -> Option<WakeRx> {
        self.wake_rx.take()
    }

    /// `pid` がアクティブタブに表示されているか。非表示タブの出力では
    /// 再描画しない (タブ切替時の描画で自然に反映される)。
    pub fn pane_visible(&self, pid: PaneId) -> bool {
        self.current_tab().layout.leaves().contains(&pid)
    }

    /// ペイン由来の状態 (Claude セッション / 実行状態 / タスク名 / git ブランチ)
    /// をまとめて更新する。イベントループの定期 tick からのみ呼ぶ。描画パスでは
    /// 呼ばない (描画側はここで作ったキャッシュを読むだけ)。
    pub fn refresh_pane_state(&mut self) {
        // 日本語タブ名の生成 (claude -p) は、再開待ちのタブを開き終えてから。
        // claude を並行起動して ~/.claude.json を同時に書かないため。
        let resumes_done = self.pending_resumes.is_empty();
        // 開発用 (CCNEST_STATUS_TRACE): 判定結果と、判定に使った画面テキストを控える。
        let mut trace = status_trace_path().map(|_| String::new());
        for (id, pane) in self.panes.iter_mut() {
            // 眠っているペイン (プロセスなし) は、最後の状態・タスク名のまま据え置く。
            if pane.asleep() {
                continue;
            }
            pane.session.refresh();
            if !pane.claude_running {
                // shell ペインに状態色・タスク名は出さない
                // (focused_model_label と同じゲート)。
                pane.status = ClaudeStatus::Idle;
                pane.task = None;
                continue;
            }
            // PTY reader スレッドはストリーミング中、チャンク処理のたびに parser
            // ロックを保持する。ここはイベントループスレッドなので lock() で
            // 待つと 1 周が PAIR_WINDOW (70ms) を超えて停滞し、ファントム矢印
            // 抑止の前提を崩す (ホイールで Claude 履歴が開く回帰の一因)。
            // try_lock で取れなければ前回の status / task を据え置き、次 tick
            // (2 秒後) に再試行する (表示専用なので staleness は無害)。
            let Ok(p) = pane.parser.try_lock() else {
                continue;
            };
            let screen = p.screen();
            // スクロールバック閲覧中は過去の画面が返るため判定しない (前回の
            // 状態を据え置き、最下部へ戻った次の tick で回復する)。
            // 画面の文字は行ごとに取る (contents() は最終列まで埋まった行を
            // 次の行とつなぐ。status::screen_text 参照)。
            if screen.scrollback() == 0 {
                let (_, cols) = screen.size();
                let text = screen_text(screen.rows(0, cols));
                pane.status = detect_status(&text);
                if let Some(t) = trace.as_mut() {
                    t.push_str(&format!(
                        "=== pane {id} status={:?} ===
{text}
",
                        pane.status
                    ));
                }
            }
            let title_task = sanitize_task(screen.title());
            drop(p);
            // 人が付けた名前 > 日本語の名前 > OSC タイトル (英語) > 最後の
            // ユーザー発話 (claude::jp_title::pick_task)。
            let info = pane.session.info();
            let allow_request = resumes_done && pane.created_at.elapsed() >= JP_TITLE_REQUEST_AFTER;
            let jp = info.ai_title.as_deref().and_then(|ai| {
                self.jp_titler.lookup(
                    &pane.session_id.to_string(),
                    ai,
                    info.task.as_deref(),
                    allow_request,
                )
            });
            pane.task = pick_task(
                info.custom_title.as_deref(),
                jp,
                title_task,
                info.task.as_deref(),
            );
        }
        if let (Some(path), Some(text)) = (status_trace_path(), trace) {
            write_status_trace(path, &text);
        }
        // ブランチは TTL キャッシュ。現存ペインの cwd だけで作り直す = 閉じた
        // ペインの分は自然に落ちる。TTL 内のエントリは引き継ぎ、切れた分だけ
        // ここで引き直す (探索自体は純粋関数の外)。
        let now = Instant::now();
        let cwds: Vec<PathBuf> = self.panes.values().map(|p| p.cwd.clone()).collect();
        let (mut next, stale) = plan_branch_refresh(&cwds, &self.branch_cache, BRANCH_TTL, now);
        for cwd in stale {
            let branch = crate::sidebar::git::branch_of(&cwd);
            next.insert(cwd, (branch, now));
        }
        self.branch_cache = next;
        // ポーラーが書き込み中なら前回値のまま、次 tick で拾う (parser と同じ)。
        if let Ok(slot) = self.usage_cell.try_lock() {
            self.usage = slot.clone();
        }
    }

    /// ステータスバーに出す利用制限: 残り時間つき (`5h 42% ⏳2h10m · wk 18% ·
    /// Fable 7% ⏳2d9h`)、% だけ (`5h 42% · wk 18% · Fable 7%`)、色の段階。
    /// アカウント全体の値なので、フォーカス中のペインが shell でも出す。
    /// 未取得・古すぎる値は None。
    pub fn usage_label(&self) -> Option<UsageLabel> {
        crate::claude::usage::status_label(self.usage.as_ref()?, chrono::Utc::now())
    }

    /// フォーカス中ペインのモデル名 (短縮表記)。claude が走っていなければ None。
    pub fn focused_model_label(&self) -> Option<String> {
        let pane = self.panes.get(&self.current_tab().focused)?;
        if !pane.claude_running {
            return None;
        }
        Some(
            pane.session
                .info()
                .model
                .as_deref()
                .map(crate::claude::session::pretty_model)
                // まだ assistant ターンが 1 回も無く、モデルが確定していない。
                .unwrap_or_else(|| "Claude …".to_string()),
        )
    }

    /// フォーカス中ペインの cwd のブランチ名。
    pub fn focused_branch(&self) -> Option<&str> {
        let cwd = self
            .panes
            .get(&self.current_tab().focused)
            .map(|p| p.cwd.as_path())
            .unwrap_or(self.cwd.as_path());
        self.branch_cache.get(cwd)?.0.as_deref()
    }

    pub fn current_tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active_tab]
    }

    pub fn current_tab(&self) -> &Tab {
        &self.tabs[self.active_tab]
    }

    pub fn focused_pane_cwd(&self) -> PathBuf {
        self.panes
            .get(&self.current_tab().focused)
            .map(|p| p.cwd.clone())
            .unwrap_or_else(|| self.cwd.clone())
    }

    pub fn split(&mut self, dir: Direction) -> Result<()> {
        let cwd = self.focused_pane_cwd();
        let new_id = self.next_pane_id;
        self.next_pane_id += 1;
        let pane = Pane::spawn(
            new_id,
            &cwd,
            Uuid::new_v4(),
            SessionStart::New,
            self.wake_tx.clone(),
        )?;
        self.panes.insert(new_id, pane);
        let focused = self.current_tab().focused;
        let layout = std::mem::replace(&mut self.current_tab_mut().layout, Layout::Leaf(focused));
        let new_layout = layout.split(focused, dir, new_id);
        self.current_tab_mut().layout = new_layout;
        self.current_tab_mut().focused = new_id;
        Ok(())
    }

    pub fn new_tab(&mut self) -> Result<()> {
        let cwd = self.focused_pane_cwd();
        let new_id = self.next_pane_id;
        self.next_pane_id += 1;
        let pane = Pane::spawn(
            new_id,
            &cwd,
            Uuid::new_v4(),
            SessionStart::New,
            self.wake_tx.clone(),
        )?;
        self.panes.insert(new_id, pane);
        self.tabs.push(Tab {
            title: folder_title(&cwd),
            user_title: None,
            parked: false,
            working_since: None,
            held: false,
            layout: Layout::Leaf(new_id),
            focused: new_id,
            slide: None,
        });
        self.active_tab = self.tabs.len() - 1;
        Ok(())
    }

    pub fn close_focused_pane(&mut self) {
        let focused = self.current_tab().focused;
        // タブ最後のペインならタブごと消えるので、閉じる前に控えを取る。
        let snapshot = (self.current_tab().layout.leaves() == [focused])
            .then(|| self.snapshot_tab(self.active_tab));
        if let Some(p) = self.panes.remove(&focused) {
            p.terminate();
        }
        let layout = std::mem::replace(&mut self.current_tab_mut().layout, Layout::Leaf(focused));
        match layout.close(focused) {
            Some(new_layout) => {
                if let Some(new_focus) = new_layout.first_leaf() {
                    self.current_tab_mut().focused = new_focus;
                }
                self.current_tab_mut().layout = new_layout;
            }
            None => {
                // Tab is empty — remove it. ペインはもう panes から抜いてあるので、
                // 控えは抜く前に取った `snapshot` を使う。
                if let Some(s) = snapshot {
                    push_closed(&mut self.closed_tabs, s);
                }
                self.remove_tab(self.active_tab);
            }
        }
    }

    /// タブ `idx` を配下の全ペインごと閉じる (確認なし。タブバーの中クリック用)。
    /// Ctrl+W (`close_focused_pane`) はフォーカス中の 1 ペインだけを閉じるのに対し、
    /// こちらは分割中のペインもまとめて terminate する。`idx` が範囲外
    /// (描画後に閉じられた古い矩形からのヒットなど) なら何もしない。
    pub fn close_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        let snapshot = self.snapshot_tab(idx);
        push_closed(&mut self.closed_tabs, snapshot);
        for pid in self.tabs[idx].layout.leaves() {
            if let Some(p) = self.panes.remove(&pid) {
                p.terminate();
            }
        }
        self.remove_tab(idx);
    }

    /// タブ `idx` の控え (タブ名・分割の形・各ペインのセッション) を取る。
    fn snapshot_tab(&self, idx: usize) -> ClosedTab {
        let tab = &self.tabs[idx];
        let panes = tab
            .layout
            .leaves()
            .into_iter()
            .filter_map(|pid| self.panes.get(&pid))
            .map(|p| ClosedPane {
                old_id: p.id,
                cwd: p.cwd.clone(),
                session_id: p.session_id,
                resumable: crate::claude::session::session_path(&p.cwd, &p.session_id.to_string())
                    .is_some_and(|path| path.is_file()),
            })
            .collect();
        ClosedTab {
            title: tab.title.clone(),
            user_title: tab.user_title.clone(),
            held: tab.held,
            index: idx,
            layout: tab.layout.clone(),
            focused: tab.focused,
            panes,
        }
    }

    /// 直前に閉じたタブを元の位置に開き直して前面に出す。会話ログのある
    /// ペインは `--resume`、無いペインは新しい claude。開き直すタブが無ければ
    /// false。途中で起動に失敗したら、作ったペインを閉じて控えを戻す。
    pub fn reopen_closed_tab(&mut self) -> Result<bool> {
        let Some(closed) = self.closed_tabs.pop() else {
            return Ok(false);
        };
        let mut spawned: Vec<(PaneId, PaneId, Pane)> = Vec::new();
        for cp in &closed.panes {
            let new_id = self.next_pane_id;
            match Pane::spawn(
                new_id,
                &cp.cwd,
                cp.session_id,
                start_for(cp.resumable),
                self.wake_tx.clone(),
            ) {
                Ok(p) => {
                    self.next_pane_id += 1;
                    spawned.push((cp.old_id, new_id, p));
                }
                Err(e) => {
                    for (_, _, p) in spawned {
                        p.terminate();
                    }
                    self.closed_tabs.push(closed);
                    return Err(e);
                }
            }
        }
        let Some(fallback) = spawned.first().map(|(_, n, _)| *n) else {
            return Ok(false);
        };
        let map: HashMap<PaneId, PaneId> = spawned.iter().map(|(o, n, _)| (*o, *n)).collect();
        // 控えに無い葉 (起こらないはず) は最初のペインへ寄せる。
        let layout = closed
            .layout
            .remap(&|id| map.get(&id).copied().unwrap_or(fallback));
        let focused = map.get(&closed.focused).copied().unwrap_or(fallback);
        for (_, new_id, p) in spawned {
            self.panes.insert(new_id, p);
        }
        let at = reopen_index(closed.index, self.tabs.len());
        self.tabs.insert(
            at,
            Tab {
                title: closed.title,
                user_title: closed.user_title,
                parked: false,
                working_since: None,
                held: closed.held,
                layout,
                focused,
                slide: None,
            },
        );
        self.active_tab = at;
        // 位置がずれるので、引いている最中の並べ替えは打ち切る。
        self.tab_drag = None;
        self.last_tab_click = None;
        Ok(true)
    }

    /// `tabs[idx]` を取り除き、`active_tab` / `quit` を更新する。
    /// `close_focused_pane` の「最後のペインを閉じた」分岐と `close_tab` で共用。
    fn remove_tab(&mut self, idx: usize) {
        self.tabs.remove(idx);
        // 位置がずれるので、引いている最中の並べ替えは打ち切る。
        self.tab_drag = None;
        // 名前の入力は前面のタブに確定する。タブが減ると前面がずれて別のタブに
        // 名前が付きかねないので、入力は取り消す。ダブルクリックの 1 回目も捨てる。
        self.renaming_tab = None;
        self.rename_fresh = false;
        self.last_tab_click = None;
        match active_after_close(self.active_tab, idx, self.tabs.len()) {
            Some(active) => self.active_tab = active,
            None => self.quit = true,
        }
    }

    /// タブ `from` を `to` の位置へ動かす (タブバーのドラッグ並べ替え用)。
    /// 間のタブは 1 つずつ寄る。アクティブタブは同じタブを指し続ける。
    /// 範囲外・同じ位置なら何もせず false。
    pub fn move_tab(&mut self, from: usize, to: usize) -> bool {
        if !move_in_vec(&mut self.tabs, from, to) {
            return false;
        }
        self.active_tab = active_after_move(self.active_tab, from, to);
        // 番号がずれるので、ダブルクリックの 1 回目は捨てる。
        self.last_tab_click = None;
        true
    }

    /// タブ `idx` に今出ている名前 (番号・マーカー抜き)。名前の入力の初期値。
    /// 手で付けた名前 > フォーカスペインのタスク名 > タブのタイトル (フォルダ名)。
    pub fn tab_display_name(&self, idx: usize) -> String {
        let Some(tab) = self.tabs.get(idx) else {
            return String::new();
        };
        let task = self.panes.get(&tab.focused).and_then(|p| p.task.as_deref());
        shown_tab_name(tab.user_title.as_deref(), task)
            .unwrap_or(&tab.title)
            .to_string()
    }

    /// 前面のタブの名前の入力を始める (Alt+F / タブのダブルクリック)。
    /// 今出ている名前が丸ごと選ばれた状態 (`rename_fresh`) で始まる。
    pub fn begin_rename_tab(&mut self) {
        self.renaming_tab = Some(self.tab_display_name(self.active_tab));
        self.rename_fresh = true;
    }

    /// 名前の入力をやめる (確定・取り消しのどちらでも呼ぶ)。
    pub fn end_rename_tab(&mut self) {
        self.renaming_tab = None;
        self.rename_fresh = false;
    }

    /// タブ `idx` に手で名前を付ける (`None` = 外して自動の名前に戻す)。
    /// そのタブの全ペインのセッション id に保存する (どのペインにフォーカスが
    /// あっても、どのペインのセッションを後で開き直しても同じ名前になるように)。
    pub fn set_tab_user_title(&mut self, idx: usize, name: Option<String>) {
        let Some(tab) = self.tabs.get_mut(idx) else {
            return;
        };
        tab.user_title = name.clone();
        let ids: Vec<String> = tab
            .layout
            .leaves()
            .into_iter()
            .filter_map(|pid| self.panes.get(&pid))
            .map(|p| p.session_id.to_string())
            .collect();
        let Some(path) = crate::tab_names::store_path() else {
            return;
        };
        match name {
            Some(n) => crate::tab_names::set(&path, &ids, &n, chrono::Utc::now().timestamp()),
            None => crate::tab_names::remove(&path, &ids),
        }
    }

    /// すべりの途中のタブがあるか (あるあいだだけイベントループが描き続ける)。
    pub fn has_tab_slides(&self) -> bool {
        self.tabs.iter().any(|t| t.slide.is_some())
    }

    /// 終わったすべりを片付ける。
    pub fn expire_tab_slides(&mut self, now: Instant) {
        for t in &mut self.tabs {
            if t.slide.is_some_and(|s| s.finished(now)) {
                t.slide = None;
            }
        }
    }

    /// `move_tab(from, to)` の直後に呼ぶ: 押しのけられたタブを、元いた位置から
    /// 新しい定位置へすべらせる。`shift` は動いた桁数 (引いたタブの幅 + 区切り 1)。
    /// すべりの途中でまた押しのけられたら、今いる所から続ける。
    pub fn slide_displaced(&mut self, from: usize, to: usize, shift: i32, now: Instant) {
        let (range, sign) = displaced_after_move(from, to);
        for i in range {
            let Some(tab) = self.tabs.get_mut(i) else {
                continue;
            };
            let current = tab.slide.map_or(0, |s| s.offset_at(now));
            tab.slide = start_slide(current + sign * shift, now);
        }
    }

    /// タブ `idx` を「定位置から `from_offset` 桁ずれた所」から定位置へすべらせる
    /// (ドラッグを離したとき、浮いていた位置からすき間へ収まる動き)。
    pub fn settle_tab(&mut self, idx: usize, from_offset: i32, now: Instant) {
        if let Some(tab) = self.tabs.get_mut(idx) {
            tab.slide = start_slide(from_offset, now);
        }
    }

    /// タブバーで右隣のタブへ (見えている並びの順。右端からは左端へ回る)。
    pub fn next_tab(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        self.active_tab = neighbor_in_order(&self.tab_order(), self.active_tab, 1);
    }

    /// タブバーで左隣のタブへ。
    pub fn prev_tab(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        self.active_tab = neighbor_in_order(&self.tab_order(), self.active_tab, -1);
    }

    /// タブの状態 = (claude ペインがあるか, 配下の全ペインの状態の畳み込み)。
    /// タブバーのマーカーと「右へ寄せるか」で共用。
    pub fn tab_status(&self, tab: &Tab) -> (bool, ClaudeStatus) {
        let panes = || {
            tab.layout
                .leaves()
                .into_iter()
                .filter_map(|pid| self.panes.get(&pid))
        };
        (
            panes().any(|p| p.claude_running),
            aggregate_status(panes().map(|p| p.status)),
        )
    }

    /// タブバーに並べる順 (タブ番号の列)。[`display_order`] 参照。
    /// `CCNEST_TAB_GROUP=off` なら並び順どおり。
    pub fn tab_order(&self) -> Vec<usize> {
        if !tab_group_enabled() {
            return (0..self.tabs.len()).collect();
        }
        display_order(&self.tab_groups())
    }

    /// 各タブの組 (タブ番号の順)。
    fn tab_groups(&self) -> Vec<TabGroup> {
        self.tabs.iter().map(Tab::group).collect()
    }

    /// タブ `idx` の保留を切り替える (Alt+C)。保留にしたタブはタブバーの右端へ
    /// 行き、灰色のマーカー + 番号だけになる。もう一度呼ぶと元の組へ戻る。
    /// 保留にしたときだけ前面を隣のタブへ移す ([`active_after_hold`])。
    /// そのタブの全ペインのセッション id で保存する。戻り値は切り替えたあとの
    /// 保留かどうか (タブが無ければ None)。
    ///
    /// 緑のタブを寄せるとき ([`App::refresh_tab_parking`]) と違い、押した瞬間に
    /// 位置を変える: 自分で押したキーなので、動くことが返事になる。
    pub fn toggle_hold_tab(&mut self, idx: usize) -> Option<bool> {
        let held = !self.tabs.get(idx)?.held;
        // 隣は保留にする前の並びで決める。CCNEST_TAB_GROUP=off では位置が
        // 変わらないので、並び順の隣になる。
        let next = held.then(|| active_after_hold(&self.tab_order(), &self.tab_groups(), idx));
        let tab = &mut self.tabs[idx];
        tab.held = held;
        // 組が変わる = 定位置が変わるので、途中のすべりは捨てる。
        tab.slide = None;
        let ids: Vec<String> = tab
            .layout
            .leaves()
            .into_iter()
            .filter_map(|pid| self.panes.get(&pid))
            .map(|p| p.session_id.to_string())
            .collect();
        // 位置がずれるので、引いている最中の並べ替えとダブルクリックの 1 回目は捨てる。
        self.tab_drag = None;
        self.last_tab_click = None;
        if let Some(next) = next {
            if idx == self.active_tab {
                self.active_tab = next;
            }
        }
        if let Some(path) = crate::tab_holds::store_path() {
            if held {
                crate::tab_holds::set(&path, &ids, chrono::Utc::now().timestamp());
            } else {
                crate::tab_holds::remove(&path, &ids);
            }
        }
        Some(held)
    }

    /// 保留にしたタブ `idx` の Claude を落としてメモリを空ける。落とすのは、
    /// いまの画面が「止めても失う物が無い」形のペインだけ ([`sleep_check`]):
    /// 実行中・裏の仕事あり・ダイアログの途中・書きかけの入力あり・入力を送った
    /// 直後のペインは動かしたまま。画面はその場で読み直す (2 秒 tick の状態は
    /// 古いことがある)。戻り値は (落とした数, 落とさなかった claude のペインの数)。
    /// `CCNEST_HOLD_SLEEP=off` なら何もしない。
    pub fn sleep_held_tab(&mut self, idx: usize) -> (usize, usize) {
        if !hold_sleep_enabled() {
            return (0, 0);
        }
        let Some(tab) = self.tabs.get(idx) else {
            return (0, 0);
        };
        let (mut stopped, mut kept) = (0, 0);
        for pid in tab.layout.leaves() {
            let Some(pane) = self.panes.get_mut(&pid) else {
                continue;
            };
            // 履歴を見ている途中だと過去の画面が返るので、最下部へ戻してから読む。
            pane.scroll_to_bottom();
            let safe = pane.parser.try_lock().ok().map(|p| {
                let screen = p.screen();
                let (_, cols) = screen.size();
                is_safe_to_stop(&screen_text(screen.rows(0, cols)))
            });
            match sleep_check(
                pane.claude_running,
                pane.asleep(),
                safe,
                pane.since_last_write(),
            ) {
                SleepCheck::Stop => {
                    pane.sleep();
                    pane.status = ClaudeStatus::NeedsAttention;
                    stopped += 1;
                }
                SleepCheck::Keep => kept += 1,
                SleepCheck::Skip => {}
            }
        }
        (stopped, kept)
    }

    /// 眠っているペイン `pid` を起こす: 同じ id・フォルダ・セッション id で claude を
    /// 起動し直して差し替える (会話ログがあれば `--resume`、無ければ新しい
    /// セッション)。眠っていなければ何もせず false。
    pub fn wake_pane(&mut self, pid: PaneId) -> Result<bool> {
        let Some(old) = self.panes.get(&pid).filter(|p| p.asleep()) else {
            return Ok(false);
        };
        let (cwd, session_id) = (old.cwd.clone(), old.session_id);
        let resumable = crate::claude::session::session_path(&cwd, &session_id.to_string())
            .is_some_and(|path| path.is_file());
        let pane = Pane::spawn(
            pid,
            &cwd,
            session_id,
            start_for(resumable),
            self.wake_tx.clone(),
        )?;
        // 古いペイン (プロセスなし) は差し替えで捨てる。
        self.panes.insert(pid, pane);
        Ok(true)
    }

    /// タブ `idx` の眠っているペインを全部起こす (保留を外したとき)。起こした数。
    pub fn wake_tab(&mut self, idx: usize) -> Result<usize> {
        let Some(tab) = self.tabs.get(idx) else {
            return Ok(0);
        };
        let mut woken = 0;
        for pid in tab.layout.leaves() {
            if self.wake_pane(pid)? {
                woken += 1;
            }
        }
        Ok(woken)
    }

    /// お知らせを出す (ステータスバーの左に [`NOTICE_TTL`] のあいだ)。
    pub fn set_notice(&mut self, text: String) {
        self.notice = Some((text, Instant::now()));
    }

    /// 期限の切れたお知らせを片付ける。片付けたら true (描き直しが要る)。
    pub fn expire_notice(&mut self, now: Instant) -> bool {
        let expired = self
            .notice
            .as_ref()
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) >= NOTICE_TTL);
        if expired {
            self.notice = None;
        }
        expired
    }

    /// 緑 (実行中／裏で実行中) が続いているタブを右へ小さく寄せ、🟨 に変わった
    /// タブを左へ戻す。2 秒 tick から `refresh_pane_state` の直後に呼ぶ。
    ///
    /// 位置が動くのはこの tick だけ: クリックした瞬間には動かさない。さらに、
    /// タブを引いている最中・名前の入力中・タブをクリックした直後 (ダブル
    /// クリックの途中かもしれない) は並びを変えず、次の tick へ回す。
    pub fn refresh_tab_parking(&mut self, now: Instant) {
        if !tab_group_enabled() {
            return;
        }
        let just_clicked = self
            .last_tab_click
            .is_some_and(|(t, _)| now.saturating_duration_since(t) <= PARK_FREEZE_AFTER_CLICK);
        if self.tab_drag.is_some() || self.renaming_tab.is_some() || just_clicked {
            return;
        }
        let working: Vec<bool> = self
            .tabs
            .iter()
            .map(|t| {
                let (has_claude, status) = self.tab_status(t);
                has_claude && matches!(status, ClaudeStatus::Busy | ClaudeStatus::Background)
            })
            .collect();
        for (tab, working) in self.tabs.iter_mut().zip(working) {
            let (since, parked) = park_step(tab.working_since, working, now);
            if parked != tab.parked {
                // 組が変わる = 定位置が変わるので、途中のすべりは捨てる。
                tab.slide = None;
            }
            tab.working_since = since;
            tab.parked = parked;
        }
    }

    /// `move_tab` の直後に呼ぶ: 引いたタブ `moved` と違う組 (名前つき／寄せた緑／保留)
    /// のタブのすべりを取り消す。`slide_displaced` はタブ番号の範囲で動かすが、
    /// 違う組のタブはタブバーの上では動いていない。
    pub fn cancel_slides_outside_group(&mut self, moved: usize) {
        let Some(group) = self.tabs.get(moved).map(Tab::group) else {
            return;
        };
        for t in &mut self.tabs {
            if t.group() != group {
                t.slide = None;
            }
        }
    }

    pub fn focus_neighbor(&mut self, dir: Direction, pane_rects: &HashMap<PaneId, Rect>) {
        let current = self.current_tab().focused;
        let Some(cur_rect) = pane_rects.get(&current).copied() else {
            return;
        };
        let mut best: Option<(PaneId, i64)> = None;
        for (pid, rect) in pane_rects.iter() {
            if *pid == current {
                continue;
            }
            let score = direction_score(&cur_rect, rect, dir);
            if let Some(s) = score {
                if best.map(|b| s < b.1).unwrap_or(true) {
                    best = Some((*pid, s));
                }
            }
        }
        if let Some((pid, _)) = best {
            self.current_tab_mut().focused = pid;
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn cx(&self) -> i32 {
        self.x + self.w / 2
    }
    pub fn cy(&self) -> i32 {
        self.y + self.h / 2
    }
}

fn direction_score(from: &Rect, to: &Rect, dir: Direction) -> Option<i64> {
    let dx = to.cx() - from.cx();
    let dy = to.cy() - from.cy();
    let ok = match dir {
        Direction::Left => dx < 0 && dx.abs() >= dy.abs(),
        Direction::Right => dx > 0 && dx.abs() >= dy.abs(),
        Direction::Up => dy < 0 && dy.abs() >= dx.abs(),
        Direction::Down => dy > 0 && dy.abs() >= dx.abs(),
    };
    if !ok {
        return None;
    }
    Some((dx.pow(2) + dy.pow(2)) as i64)
}

/// 純粋: 現存ペインの cwd 列と既存キャッシュから、(TTL 内で引き継ぐエントリ,
/// 再探索すべき cwd 列) を決める。閉じたペインの cwd は引き継がれず自然に落ち、
/// 重複 cwd は 1 回だけ再探索する。`App` 非依存 (実 PTY spawn なし) で単体テスト
/// 可能にするため探索そのもの (`git::branch_of`) は呼び出し側で行う。
/// 開発用: `CCNEST_STATUS_TRACE=<path>` のとき、2 秒 tick ごとに Claude ペインの
/// 判定結果と、判定に使った画面テキスト (vt100 の行を status::screen_text で
/// つないだもの) を `<path>` に書く。
/// 状態判定のテスト文字列を実物から起こすため (v0.1.31〜33 は conhost 越しに
/// 読んで空白が潰れた文字列でテストを書き、実物に一度も当たらなかった)。
/// 未設定なら OnceLock の判定だけ。
fn status_trace_path() -> Option<&'static Path> {
    use std::sync::OnceLock;
    static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| {
        std::env::var_os("CCNEST_STATUS_TRACE")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    })
    .as_deref()
}

/// 読む側が書きかけを見ないよう、隣の一時ファイルに書いてから置き換える。
fn write_status_trace(path: &Path, text: &str) {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

fn plan_branch_refresh(
    cwds: &[PathBuf],
    cache: &BranchCache,
    ttl: Duration,
    now: Instant,
) -> (BranchCache, Vec<PathBuf>) {
    let mut next = BranchCache::new();
    let mut stale: Vec<PathBuf> = Vec::new();
    for cwd in cwds {
        if next.contains_key(cwd) || stale.contains(cwd) {
            continue;
        }
        match cache.get(cwd) {
            Some((branch, at)) if now.saturating_duration_since(*at) < ttl => {
                next.insert(cwd.clone(), (branch.clone(), *at));
            }
            _ => stale.push(cwd.clone()),
        }
    }
    (next, stale)
}

/// 純粋: 閉じたタブを積む。上限を超えたら古いもの (先頭) から捨てる。
fn push_closed(stack: &mut Vec<ClosedTab>, tab: ClosedTab) {
    stack.push(tab);
    if stack.len() > CLOSED_TABS_MAX {
        let extra = stack.len() - CLOSED_TABS_MAX;
        stack.drain(..extra);
    }
}

/// 純粋: 開き直すタブの位置。元の位置、今のタブ数を超えるなら末尾。
fn reopen_index(index: usize, len: usize) -> usize {
    index.min(len)
}

/// 純粋: 会話ログがあれば `--resume`、無ければ新しいセッション。
/// 純粋: `--resume` 1 つ分の指定から、そのセッションだけに付ける起動指定を取り出す。
fn resume_extras(spec: &ResumeSpec) -> ResumeExtras {
    ResumeExtras {
        prompt: spec.prompt.clone(),
        permission_mode: spec.permission_mode.clone(),
    }
}

/// 純粋: `--resume` / `--add` で開くセッションを、claude を起こさず眠ったまま
/// 開くか。保留として保存されていて、最初の一言 (続きの指示) が無く、
/// メモリを空ける機能が有効なとき。
fn opens_dormant(held: bool, has_prompt: bool, sleep_enabled: bool) -> bool {
    held && !has_prompt && sleep_enabled
}

fn start_for(resumable: bool) -> SessionStart {
    if resumable {
        SessionStart::Resume
    } else {
        SessionStart::New
    }
}

/// 純粋: `v[from]` を `to` の位置へ移す (間の要素は 1 つずつ寄る)。
/// 範囲外・同じ位置なら何もせず false。
fn move_in_vec<T>(v: &mut Vec<T>, from: usize, to: usize) -> bool {
    if from == to || from >= v.len() || to >= v.len() {
        return false;
    }
    let item = v.remove(from);
    v.insert(to, item);
    true
}

/// 純粋: ずれ 0 ならすべらせない。
fn start_slide(from_offset: i32, now: Instant) -> Option<TabSlide> {
    (from_offset != 0).then_some(TabSlide {
        from_offset,
        start: now,
    })
}

/// 純粋: すべりの途中のずれ。始まりは `from_offset`、`total` 経ったら 0。
/// ease-out (3 乗) = 動き出しが速く、止まり際がゆっくり。向きは変わらない。
pub fn slide_offset(from_offset: i32, elapsed: Duration, total: Duration) -> i32 {
    if total.is_zero() || elapsed >= total {
        return 0;
    }
    let left = 1.0 - elapsed.as_secs_f32() / total.as_secs_f32();
    (from_offset as f32 * left * left * left).round() as i32
}

/// 純粋: `move_tab(from, to)` のあと、押しのけられたタブが今いる位置の範囲と、
/// すべり始めのずれの向き。右へ動かした (`from < to`) なら間のタブは左へ寄ったので
/// 元の位置は右 = +1。左へ動かしたなら -1。
fn displaced_after_move(from: usize, to: usize) -> (std::ops::Range<usize>, i32) {
    match from.cmp(&to) {
        std::cmp::Ordering::Less => (from..to, 1),
        std::cmp::Ordering::Greater => (to + 1..from + 1, -1),
        std::cmp::Ordering::Equal => (from..from, 0),
    }
}

/// 純粋: 浮いたタブを描く桁と、その定位置 (すき間) の桁 `(float_x, slot_x)`。
/// 浮いたタブは「ポインタ − 押したときのタブ内の位置」に置き、見えているタブの帯
/// (先頭タブの左端〜末尾タブの右端) からはみ出さない。`tab_rects` は描画が記録した
/// 定位置の矩形。引いているタブが描かれていなければ None。
/// 描画 (浮かせる位置) とイベント (離したときの収まり始め) が同じ値を使う。
pub fn float_in_strip(tab_rects: &[(Rect, usize)], drag: &TabDrag) -> Option<(i32, i32)> {
    let slot = tab_rects.iter().find(|(_, i)| *i == drag.idx)?.0;
    let start = tab_rects.iter().map(|(r, _)| r.x).min()?;
    let end = tab_rects.iter().map(|(r, _)| r.x + r.w).max()?;
    let x = (drag.pointer_x - drag.grab_dx).clamp(start, (end - slot.w).max(start));
    Some((x, slot.x))
}

/// 純粋: タブ `from` を `to` へ動かした後のアクティブ index (同じタブを指し続ける)。
/// 動かしたタブ自身なら `to`。アクティブをまたいで右へ動いたら 1 つ左へ、
/// 左へ動いたら 1 つ右へ。またがなければそのまま。
fn active_after_move(active: usize, from: usize, to: usize) -> usize {
    if active == from {
        to
    } else if from < active && active <= to {
        active - 1
    } else if to <= active && active < from {
        active + 1
    } else {
        active
    }
}

/// 純粋: タブ `closed` を取り除いた後 (残り `remaining` 枚) のアクティブ index。
/// `closed < active` なら 1 つ左へ、`closed == active` なら同 index (右隣が滑り込む)、
/// 末尾を超えたら末尾へクランプ。`remaining == 0` なら None (= quit)。
fn active_after_close(active: usize, closed: usize, remaining: usize) -> Option<usize> {
    if remaining == 0 {
        return None;
    }
    let next = if closed < active { active - 1 } else { active };
    Some(next.min(remaining - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn closed(title: &str) -> ClosedTab {
        ClosedTab {
            title: title.to_string(),
            user_title: None,
            held: false,
            index: 0,
            layout: Layout::Leaf(1),
            focused: 1,
            panes: Vec::new(),
        }
    }

    /// 閉じたタブは新しい順に取り出せ、上限を超えたら古いものから捨てる。
    #[test]
    fn push_closed_caps_and_pops_newest_first() {
        let mut stack = Vec::new();
        for i in 0..(CLOSED_TABS_MAX + 3) {
            push_closed(&mut stack, closed(&format!("t{i}")));
        }
        assert_eq!(stack.len(), CLOSED_TABS_MAX);
        assert_eq!(stack.first().unwrap().title, "t3");
        assert_eq!(
            stack.pop().unwrap().title,
            format!("t{}", CLOSED_TABS_MAX + 2)
        );
    }

    /// 開き直す位置は元の位置。今のタブ数を超えるなら末尾。
    #[test]
    fn reopen_index_clamps_to_end() {
        assert_eq!(reopen_index(1, 3), 1);
        assert_eq!(reopen_index(3, 3), 3);
        assert_eq!(reopen_index(5, 2), 2);
        assert_eq!(reopen_index(0, 0), 0);
    }

    /// 会話ログがあれば --resume、無ければ新しいセッション。
    #[test]
    fn start_for_resumable() {
        assert_eq!(start_for(true), SessionStart::Resume);
        assert_eq!(start_for(false), SessionStart::New);
    }

    const TTL: Duration = Duration::from_secs(10);

    /// タブ名とウィンドウタイトル (タスクバー) はフォルダ名。ドライブ直下はパスのまま。
    #[test]
    fn folder_title_is_last_component() {
        assert_eq!(
            folder_title(Path::new(r"C:\work\ClaudeCompany")),
            "ClaudeCompany"
        );
        assert_eq!(folder_title(Path::new(r"C:\work\26_XTP3\")), "26_XTP3");
        assert_eq!(folder_title(Path::new("/")), "/");
    }

    fn cache_with(cwd: &str, branch: Option<&str>, at: Instant) -> BranchCache {
        let mut c = BranchCache::new();
        c.insert(PathBuf::from(cwd), (branch.map(str::to_string), at));
        c
    }

    /// タブ削除後のアクティブ index: 左が消えたら 1 つ左へ、自身が消えたら右隣が
    /// 滑り込む (同 index)、末尾超えはクランプ、右が消えても不変、0 枚なら quit。
    #[test]
    fn active_after_close_table() {
        assert_eq!(
            active_after_close(2, 0, 2),
            Some(1),
            "左が消えたら 1 つ左へ"
        );
        assert_eq!(
            active_after_close(1, 1, 2),
            Some(1),
            "自身が消えたら右隣が同 index に"
        );
        assert_eq!(
            active_after_close(2, 2, 2),
            Some(1),
            "末尾のアクティブは末尾へクランプ"
        );
        assert_eq!(active_after_close(0, 1, 1), Some(0), "右が消えても不変");
        assert_eq!(active_after_close(0, 0, 0), None, "最後の 1 枚 → quit");
    }

    /// タブ移動後のアクティブ index: 動かしたタブがアクティブならついていく。
    /// アクティブをまたいで右へ動いたら 1 つ左へ、左へ動いたら 1 つ右へ。
    /// またがない移動では不変。
    /// 緑が 4 秒続いたら右へ寄せる。緑でなくなったらすぐ戻す (起点も消す)。
    #[test]
    fn park_step_parks_after_the_grace_and_unparks_at_once() {
        let t0 = Instant::now();
        let s = |n: u64| t0 + Duration::from_secs(n);
        assert_eq!(park_step(None, false, t0), (None, false));
        assert_eq!(
            park_step(None, true, t0),
            (Some(t0), false),
            "緑になった最初の tick"
        );
        assert_eq!(
            park_step(Some(t0), true, s(2)),
            (Some(t0), false),
            "2 秒ではまだ"
        );
        assert_eq!(
            park_step(Some(t0), true, s(4)),
            (Some(t0), true),
            "4 秒続いたら寄せる"
        );
        assert_eq!(park_step(Some(t0), true, s(600)), (Some(t0), true));
        assert_eq!(
            park_step(Some(t0), false, s(601)),
            (None, false),
            "🟨 に変わったら待たずに戻す"
        );
        assert_eq!(
            park_step(None, true, s(602)),
            (Some(s(602)), false),
            "また緑になったら数え直す"
        );
    }

    /// 落とすのは「止めてよい画面」で、入力を送った直後でない claude のペインだけ。
    #[test]
    fn sleep_check_stops_only_when_it_is_known_to_be_safe() {
        let long_ago = Some(Duration::from_secs(60));
        assert_eq!(sleep_check(true, false, Some(true), None), SleepCheck::Stop);
        assert_eq!(
            sleep_check(true, false, Some(true), long_ago),
            SleepCheck::Stop
        );
        assert_eq!(
            sleep_check(true, false, Some(false), long_ago),
            SleepCheck::Keep,
            "実行中・ダイアログ・書きかけ"
        );
        assert_eq!(
            sleep_check(true, false, None, long_ago),
            SleepCheck::Keep,
            "画面を読めなかったら落とさない"
        );
        assert_eq!(
            sleep_check(true, false, Some(true), Some(Duration::from_secs(2))),
            SleepCheck::Keep,
            "入力を送った直後は、画面が空の入力欄でも落とさない"
        );
        assert_eq!(
            sleep_check(true, false, Some(true), Some(SLEEP_QUIET_AFTER_INPUT)),
            SleepCheck::Stop
        );
        assert_eq!(
            sleep_check(false, false, Some(true), None),
            SleepCheck::Skip,
            "shell のペインは落とさない (数えない)"
        );
        assert_eq!(
            sleep_check(true, true, Some(true), None),
            SleepCheck::Skip,
            "もう眠っている"
        );
    }

    #[test]
    fn hold_notice_says_what_happened() {
        assert_eq!(hold_notice(0, 0), None, "shell だけのタブ");
        assert!(hold_notice(1, 0).unwrap().contains("メモリを空けました"));
        assert!(hold_notice(2, 0).unwrap().contains("メモリを空けました"));
        assert!(hold_notice(0, 1).unwrap().contains("動かしたまま"));
        let mixed = hold_notice(1, 2).unwrap();
        assert!(
            mixed.contains("1 個止めました") && mixed.contains("2 個"),
            "{mixed}"
        );
    }

    /// 眠ったまま開くのは、保留として保存されていて、最初の一言が無いときだけ。
    #[test]
    fn opens_dormant_only_for_a_held_session_without_a_prompt() {
        assert!(opens_dormant(true, false, true));
        assert!(!opens_dormant(false, false, true), "保留でない");
        assert!(!opens_dormant(true, true, true), "続きの指示がある");
        assert!(!opens_dormant(true, false, false), "CCNEST_HOLD_SLEEP=off");
    }

    const L: TabGroup = TabGroup::Left;
    const W: TabGroup = TabGroup::Working;
    const H: TabGroup = TabGroup::Held;

    /// 寄せていないタブが左・寄せたタブが右。どちらも元の並びの順。
    #[test]
    fn display_order_keeps_each_group_in_tab_order() {
        assert_eq!(display_order(&[W, L, W, W, L]), [1, 4, 0, 2, 3]);
        assert_eq!(display_order(&[L, L, L]), [0, 1, 2]);
        assert_eq!(display_order(&[W, W]), [0, 1]);
        assert!(display_order(&[]).is_empty());
    }

    /// 保留はいちばん右 (緑で寄せた組よりも右)。組の中は元の並びの順。
    #[test]
    fn display_order_puts_held_tabs_last() {
        assert_eq!(display_order(&[H, L, W, H, L, W]), [1, 4, 2, 5, 0, 3]);
        assert_eq!(display_order(&[H, L]), [1, 0]);
        assert_eq!(display_order(&[H, H]), [0, 1]);
    }

    /// 保留は状態より優先: 緑が続いて寄せてあっても保留の組。
    #[test]
    fn tab_group_held_wins_over_parked() {
        assert_eq!(tab_group(false, false), L);
        assert_eq!(tab_group(false, true), W);
        assert_eq!(tab_group(true, false), H);
        assert_eq!(tab_group(true, true), H);
    }

    /// 保留にしたら次へ進む: 同じ組の右隣 → 左隣 → 保留でないタブ → そのまま。
    #[test]
    fn active_after_hold_moves_to_the_neighbour() {
        // タブバーの並び: 0, 1, 2 (名前つき) │ 3 (緑) │ 4 (保留)。
        let groups = [L, L, L, W, H];
        let order = display_order(&groups);
        assert_eq!(active_after_hold(&order, &groups, 0), 1, "同じ組の右隣");
        assert_eq!(active_after_hold(&order, &groups, 1), 2);
        assert_eq!(
            active_after_hold(&order, &groups, 2),
            1,
            "組の右端なら左隣 (緑の組へは行かない)"
        );
        assert_eq!(
            active_after_hold(&order, &groups, 3),
            2,
            "緑の組に 1 枚だけなら、名前つきの組のいちばん近いタブ"
        );
        // 名前つきが 1 枚だけ: 右の緑へ。
        let groups = [W, L, H];
        let order = display_order(&groups);
        assert_eq!(order, [1, 0, 2]);
        assert_eq!(active_after_hold(&order, &groups, 1), 0);
        // ほかが全部保留: そのまま。
        let groups = [H, L, H];
        let order = display_order(&groups);
        assert_eq!(active_after_hold(&order, &groups, 1), 1);
        assert_eq!(active_after_hold(&[0], &[L], 0), 0, "1 枚だけ");
        assert_eq!(
            active_after_hold(&order, &groups, 9),
            9,
            "無いタブは動かさない"
        );
    }

    /// 並びを変えない設定 (CCNEST_TAB_GROUP=off) では、並び順の隣へ進む。
    #[test]
    fn active_after_hold_in_plain_tab_order() {
        let groups = [L, H, L, L];
        let order = [0, 1, 2, 3];
        assert_eq!(
            active_after_hold(&order, &groups, 0),
            2,
            "保留を飛ばして右へ"
        );
        assert_eq!(active_after_hold(&order, &groups, 3), 2, "右端なら左へ");
    }

    /// 隣のタブ = タブバーに見えている並びの隣。端は回る。
    #[test]
    fn neighbor_in_order_follows_the_tab_bar() {
        let order = [1, 4, 0, 2, 3];
        assert_eq!(neighbor_in_order(&order, 1, 1), 4);
        assert_eq!(neighbor_in_order(&order, 4, 1), 0, "左の組の端から右の組へ");
        assert_eq!(neighbor_in_order(&order, 3, 1), 1, "右端から左端へ回る");
        assert_eq!(neighbor_in_order(&order, 1, -1), 3, "左端から右端へ回る");
        assert_eq!(neighbor_in_order(&order, 0, -1), 4);
        assert_eq!(
            neighbor_in_order(&order, 9, 1),
            9,
            "並びに無ければ動かさない"
        );
        assert_eq!(neighbor_in_order(&[0], 0, 1), 0);
    }

    #[test]
    fn active_after_move_table() {
        assert_eq!(active_after_move(0, 0, 2), 2, "動かしたタブについていく");
        assert_eq!(active_after_move(2, 2, 0), 0, "左へ動かしてもついていく");
        assert_eq!(
            active_after_move(1, 0, 2),
            0,
            "左のタブが右へまたいだら 1 つ左へ"
        );
        assert_eq!(active_after_move(2, 0, 2), 1, "移動先の位置にいたら左へ");
        assert_eq!(
            active_after_move(1, 2, 0),
            2,
            "右のタブが左へまたいだら 1 つ右へ"
        );
        assert_eq!(active_after_move(0, 2, 0), 1, "移動先の位置にいたら右へ");
        assert_eq!(active_after_move(3, 0, 2), 3, "移動範囲の右外は不変");
        assert_eq!(active_after_move(0, 1, 3), 0, "移動範囲の左外は不変");
        assert_eq!(active_after_move(1, 2, 2), 1, "同じ位置への移動は不変");
    }

    /// `move_in_vec` は from の要素を to の位置へ移し、間の要素を 1 つずつ寄せる。
    /// 範囲外・同じ位置は何もせず false。
    #[test]
    fn move_in_vec_rotates_between_from_and_to() {
        let mut v = vec!['a', 'b', 'c', 'd'];
        assert!(move_in_vec(&mut v, 0, 2));
        assert_eq!(v, vec!['b', 'c', 'a', 'd']);
        assert!(move_in_vec(&mut v, 3, 0));
        assert_eq!(v, vec!['d', 'b', 'c', 'a']);
        assert!(!move_in_vec(&mut v, 1, 1), "同じ位置");
        assert!(!move_in_vec(&mut v, 4, 0), "from が範囲外");
        assert!(!move_in_vec(&mut v, 0, 4), "to が範囲外");
        assert_eq!(v, vec!['d', 'b', 'c', 'a']);
    }

    /// すべりのずれは、始まりで元のずれ・終わりで 0。途中は元の向きのまま縮む。
    #[test]
    fn slide_offset_eases_to_zero() {
        let total = Duration::from_millis(140);
        let at = |from: i32, ms: u64| slide_offset(from, Duration::from_millis(ms), total);
        assert_eq!(at(12, 0), 12, "始まりは元のずれ");
        assert_eq!(at(12, 140), 0, "終わりは 0");
        assert_eq!(at(12, 999), 0, "過ぎても 0");
        assert_eq!(at(-12, 0), -12);
        assert_eq!(at(-12, 140), 0);
        assert_eq!(at(0, 50), 0, "ずれが無ければ動かない");
        // 途中は単調に縮み、向きは変わらない。ease-out = 前半で大きく動く。
        let mut prev = 12;
        for ms in (0..=140).step_by(10) {
            let v = at(12, ms);
            assert!((0..=prev).contains(&v), "ms={ms} v={v} prev={prev}");
            assert_eq!(at(-12, ms), -v, "左右対称 ms={ms}");
            prev = v;
        }
        assert!(
            at(12, 70) <= 3,
            "半分の時間で 3/4 以上進んでいる: {}",
            at(12, 70)
        );
        assert_eq!(
            slide_offset(12, Duration::ZERO, Duration::ZERO),
            0,
            "時間 0"
        );
    }

    /// `move_tab(from, to)` のあと、押しのけられたタブの位置と、ずらす向き。
    #[test]
    fn displaced_after_move_ranges() {
        // 0 → 2: 今 0, 1 にいるタブは左へ寄った → 元の位置 (右) から滑らせる = +。
        assert_eq!(displaced_after_move(0, 2), (0..2, 1));
        // 2 → 0: 今 1, 2 にいるタブは右へ寄った → 元の位置 (左) から = -。
        assert_eq!(displaced_after_move(2, 0), (1..3, -1));
        assert_eq!(displaced_after_move(1, 2), (1..2, 1));
        assert_eq!(displaced_after_move(1, 1), (1..1, 0), "動かない");
    }

    fn rects3() -> Vec<(Rect, usize)> {
        let r = |x: i32, w: i32| Rect { x, y: 0, w, h: 1 };
        // サイドバーの右 (x=20) から: 幅 10 / 20 / 6。帯は 20..58。
        vec![(r(20, 10), 0usize), (r(31, 20), 1usize), (r(52, 6), 2usize)]
    }

    fn drag(idx: usize, press_x: i32, grab_dx: i32, pointer_x: i32) -> TabDrag {
        TabDrag {
            idx,
            press_x,
            grab_dx,
            pointer_x,
            floating: true,
        }
    }

    /// 浮いたタブは「ポインタ − 押したときのタブ内の位置」に来る。タブの帯の外へは出ない。
    #[test]
    fn float_in_strip_follows_pointer_and_clamps() {
        let rects = rects3();
        // タブ 0 (x=20) の 3 桁目を押して 5 桁右へ → 25。すき間は 20 のまま。
        assert_eq!(float_in_strip(&rects, &drag(0, 23, 3, 28)), Some((25, 20)));
        // 左へ引いても帯の左端 (20) で止まる。
        assert_eq!(float_in_strip(&rects, &drag(0, 23, 3, 2)), Some((20, 20)));
        // 右は「帯の右端 − 自分の幅」= 58 − 10 = 48 で止まる。
        assert_eq!(float_in_strip(&rects, &drag(0, 23, 3, 200)), Some((48, 20)));
        // タブ 2 (x=52・幅 6) を左へ。
        assert_eq!(float_in_strip(&rects, &drag(2, 54, 2, 40)), Some((38, 52)));
        // 描かれていないタブ・空の矩形は None。
        assert_eq!(float_in_strip(&rects, &drag(7, 0, 0, 0)), None);
        assert_eq!(float_in_strip(&[], &drag(0, 0, 0, 0)), None);
    }

    /// TTL 内のエントリは再探索リストへ入れず、値と時刻ごと引き継ぐ。
    #[test]
    fn plan_branch_refresh_carries_fresh_entry_without_rediscovery() {
        let now = Instant::now();
        let cache = cache_with("C:/repo", Some("main"), now);
        let (next, stale) = plan_branch_refresh(&[PathBuf::from("C:/repo")], &cache, TTL, now);
        assert!(stale.is_empty(), "stale: {stale:?}");
        assert_eq!(
            next.get(Path::new("C:/repo")).unwrap().0.as_deref(),
            Some("main")
        );
    }

    /// 経過 >= TTL のエントリは再探索リストへ (境界は再探索側に倒す)。
    #[test]
    fn plan_branch_refresh_lists_stale_entry() {
        let now = Instant::now();
        let cache = cache_with("C:/repo", Some("main"), now - TTL);
        let (next, stale) = plan_branch_refresh(&[PathBuf::from("C:/repo")], &cache, TTL, now);
        assert!(next.is_empty(), "next: {next:?}");
        assert_eq!(stale, vec![PathBuf::from("C:/repo")]);
    }

    /// キャッシュに無い cwd は再探索リストへ。
    #[test]
    fn plan_branch_refresh_lists_uncached_cwd() {
        let now = Instant::now();
        let cache = BranchCache::new();
        let (next, stale) = plan_branch_refresh(&[PathBuf::from("C:/new")], &cache, TTL, now);
        assert!(next.is_empty());
        assert_eq!(stale, vec![PathBuf::from("C:/new")]);
    }

    /// 閉じたペインの cwd (cwds に現存しないキャッシュ) は引き継がず自然に落ちる。
    #[test]
    fn plan_branch_refresh_drops_closed_pane_cwd() {
        let now = Instant::now();
        let cache = cache_with("C:/closed", Some("main"), now);
        let (next, stale) = plan_branch_refresh(&[PathBuf::from("C:/alive")], &cache, TTL, now);
        assert!(!next.contains_key(Path::new("C:/closed")));
        assert_eq!(stale, vec![PathBuf::from("C:/alive")]);
    }

    /// 同一 cwd を共有する複数ペインでも再探索は 1 回だけ。
    #[test]
    fn plan_branch_refresh_dedupes_shared_cwd() {
        let now = Instant::now();
        let cache = BranchCache::new();
        let cwds = [PathBuf::from("C:/repo"), PathBuf::from("C:/repo")];
        let (next, stale) = plan_branch_refresh(&cwds, &cache, TTL, now);
        assert!(next.is_empty());
        assert_eq!(stale, vec![PathBuf::from("C:/repo")]);
    }
}

// Version History
// ver0.1 - 2026-08-11 - refresh_pane_state: parser.lock() → try_lock() (skip on
//                       contention, keep previous status/task) so the 2s tick
//                       never blocks the event loop behind the PTY reader
//                       thread; git branch discovery moved to a 10s TTL cache
//                       (branch_cache now stores (branch, discovered_at);
//                       pure plan_branch_refresh() decides carry-over vs
//                       re-discovery). Both stalls broke the PAIR_WINDOW
//                       assumption of the phantom-arrow coalescer (wheel
//                       reopened Claude prompt history).
// ver0.2 - 2026-09-06 - Own the wake channel (WakeTx handed to every Pane's
//                       reader, WakeRx taken by run_event_loop), pane_visible()
//                       for output-driven redraw gating, and the
//                       CCNEST_LATENCY_TRACE bookkeeping fields.
// ver0.3 - 2026-09-07 - Hold echo_pending (last key write to the focused pane,
//                       so the echo frame can skip the output cap) and
//                       last_forwarded_move (cell-granularity dedupe of motion
//                       reports sent to a mouse-tracking child).
// ver0.4 - 2026-09-26 - close_tab(idx) terminates every pane of a tab (tab-bar
//                       middle-click); remove_tab() + pure active_after_close()
//                       are shared with close_focused_pane, which now also marks
//                       the newly active tab seen.
// ver0.5 - 2026-10-06 - usage_cell (filled by the usage poller thread) is copied
//                       into usage on the 2 s tick with try_lock; usage_label()
//                       gives the status-bar text and level, None when missing
//                       or older than 30 minutes.
// ver0.6 - 2026-10-10 - usage_label() returns UsageLabel { full, bare, level }:
//                       the status-bar text with the countdown to each reset
//                       and the %-only fallback for narrow rows.
// ver0.7 - 2026-10-19 - with_resumes(cwd, resumes): the first saved session
//                       opens in the first tab, the rest queue in
//                       pending_resumes and spawn_next_resume() opens one tab
//                       per 2 s tick without stealing the active tab.
// ver0.8 - 2026-10-24 - refresh_pane_state: pane.task = pick_task(custom-title,
//                       日本語タブ名 (jp_titler), OSC タイトル, 最後の発話)。
//                       生成の依頼は --resume のタブを開き終え、claude 起動から
//                       10 秒 (JP_TITLE_REQUEST_AFTER) 経ってから。
// ver0.9 - 2026-10-31 - Ctrl+Shift+T reopens closed tabs: close_tab and
//                       close_focused_pane (when the tab becomes empty) push a
//                       ClosedTab (title, index, layout, focused, panes with
//                       session id / cwd / resumable = the session log exists)
//                       onto closed_tabs (max 20). reopen_closed_tab respawns
//                       each pane (--resume when resumable), remaps the layout
//                       to the new pane ids and inserts the tab at its old index.
// ver0.10 - 2026-11-08 - Two-state status: refresh_pane_state calls
//                        detect_status(screen) only (no prev / tab_active);
//                        mark_active_tab_seen and the DoneUnseen bookkeeping
//                        are gone (idle and finished both mean "your turn").
// ver0.11 - 2026-11-11 - `ccnest --add`: App owns a remote::Inbox (per-pid
//                        request folder, removed on drop) and
//                        accept_remote_requests() queues the requested
//                        sessions into pending_resumes on the 2 s tick,
//                        skipping ones already open here or without a
//                        session log in this window's folder, and writes
//                        the reply file.
// ver0.13 - 2026-11-14 - detect_status can return Background (🔵). Dev aid
//                        CCNEST_STATUS_TRACE=<path>: every 2 s tick writes each
//                        Claude pane's status and the screen text it was judged
//                        from (source of the real strings in status.rs tests).
// ver0.14 - 2026-11-15 - Drag a tab to reorder: tab_drag holds the dragged
//                        tab's current index, move_tab(from, to) rotates the
//                        tabs between the two positions (pure move_in_vec) and
//                        keeps active_tab on the same tab (pure
//                        active_after_move). remove_tab / reopen_closed_tab
//                        drop tab_drag because they shift the indexes.
// ver0.15 - 2026-11-15 - Floating tab drag: tab_drag is a TabDrag (index, press
//                        column, grab offset, pointer column, floating) and
//                        Tab::slide a TabSlide (offset from the slot easing to
//                        0 over TAB_SLIDE = 140 ms, pure slide_offset).
//                        slide_displaced() starts the slide of the tabs pushed
//                        aside by move_tab, settle_tab() the drop; pure
//                        float_in_strip() gives the floating tab's column for
//                        both the draw and the drop. CCNEST_TAB_ANIM=off
//                        disables it (tab_anim_enabled).
// ver0.16 - 2026-11-15 - refresh_pane_state judges the status from the screen
//                        taken row by row (Screen::rows via status::screen_text)
//                        instead of Screen::contents(), which glues a row that
//                        is filled to the last column onto the next one (a
//                        heading and its first item became one line).
// ver0.17 - 2026-11-18 - App::click_anchor (ClickAnchor): the last plain left
//                        click in a pane, in buffer coordinates, with the
//                        pane's alt-screen flag, reset generation and size at
//                        that moment. It is the start point when Alt / Shift +
//                        click extends a selection and nothing is selected.
// ver0.18 - 2026-11-21 - Tab.user_title: a name given by hand (double-click a
//                        tab / Alt+F). It is shown ahead of the Claude task
//                        name (shown_tab_name), kept in ClosedTab, saved per
//                        session id through tab_names (set_tab_user_title)
//                        and looked up when a tab is opened with --resume /
//                        --add. rename_fresh / last_tab_click hold the input
//                        and double-click state; move_tab / remove_tab /
//                        reopen_closed_tab drop last_tab_click and remove_tab
//                        cancels a rename in progress.
// ver0.19 - 2026-11-21 - with_resumes / spawn_next_resume pass each ResumeSpec's
//                        prompt and permission mode to the pane (resume_extras,
//                        Pane::spawn_with): `--resume-prompt-file` /
//                        `--resume-permission-mode`.
// ver0.20 - 2026-11-21 - Tab.parked / working_since: a tab that has been
//                        working (🟢) for PARK_AFTER (4 s) is parked on the
//                        right of the tab bar and drawn small; it returns as
//                        soon as it needs attention. Pure park_step /
//                        display_order / neighbor_in_order; tab_order() is
//                        the tab-bar order (next_tab / prev_tab follow it).
//                        refresh_tab_parking() changes groups only on the
//                        2 s tick and never during a tab drag, a rename or
//                        within 600 ms of a tab click. tab_status() is
//                        shared with the tab marker. CCNEST_TAB_GROUP=off.
// ver0.21 - 2026-11-21 - Tab.held (Alt+C): a tab on hold sits at the far right
//                        of the tab bar as a grey marker + number until Alt+C
//                        is pressed again, whatever its status. TabGroup
//                        { Left, Working, Held } / pure tab_group (held wins
//                        over parked); display_order takes the groups.
//                        toggle_hold_tab() flips it at once, moves the front
//                        to the neighbour when a tab is put on hold (pure
//                        active_after_hold) and saves it per session id
//                        through tab_holds; ClosedTab keeps it and tabs opened
//                        with --resume / --add look it up.
// ver0.22 - 2026-11-21 - A tab put on hold frees its memory: sleep_held_tab()
//                        kills the Claude of every pane whose screen is safe
//                        to stop right now (status::is_safe_to_stop, read
//                        fresh; pure sleep_check also keeps a pane that was
//                        sent input in the last 5 s). wake_pane() / wake_tab()
//                        respawn with --resume in the same pane id.
//                        refresh_pane_state skips sleeping panes. A held
//                        session opened with --resume / --add (no first
//                        prompt) starts dormant, without a process
//                        (opens_dormant). App::notice is a status-bar
//                        message that expires (NOTICE_TTL).
//                        CCNEST_HOLD_SLEEP=off turns the feature off.
