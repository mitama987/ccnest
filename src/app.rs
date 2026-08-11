use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;
use uuid::Uuid;

use crate::pane::grid::{Direction, Layout};
use crate::pane::status::{detect_status, sanitize_task, ClaudeStatus};
use crate::pane::{Pane, PaneId};
use crate::sidebar::SidebarState;

pub struct Tab {
    pub title: String,
    pub layout: Layout,
    pub focused: PaneId,
}

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
    pub renaming_tab: Option<String>,
    /// 直近の Ctrl+C の (ペイン, 押下時刻)。同ペインで閾値内に再度 Ctrl+C が
    /// 来たら claude→shell の再起動をトリガする。
    pub last_ctrl_c: Option<(PaneId, Instant)>,
    /// マウスドラッグで作られた画面テキスト選択範囲。選択がある状態で Ctrl+C を
    /// 押すとクリップボードへコピーされる。
    pub selection: Option<Selection>,
    /// 直近の左クリックの (押下時刻, ペイン, ペイン内 row)。閾値内に同じペインの
    /// 同じ行で再クリックされたらダブルクリックと判定し、その行を全選択する。
    pub last_left_click: Option<(Instant, PaneId, u16)>,
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
    /// cwd -> ブランチ名。複数ペインが同じ cwd を共有しがちなので、リポジトリ
    /// 探索の重複を避けるため App 側に持つ。`refresh_pane_state` が現存ペインの
    /// cwd だけで毎回作り直すので、ペインを閉じてもエントリが残らない。
    pub branch_cache: HashMap<PathBuf, Option<String>>,
}

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

/// Derive a tab title from the pane cwd (final path component).
pub fn folder_title(cwd: &Path) -> String {
    cwd.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| cwd.display().to_string())
}

impl App {
    pub fn new(cwd: PathBuf) -> Result<Self> {
        let mut panes = HashMap::new();
        let first_id = 1 as PaneId;
        let first = Pane::spawn(first_id, &cwd, Uuid::new_v4())?;
        panes.insert(first_id, first);

        let tab = Tab {
            title: folder_title(&cwd),
            layout: Layout::Leaf(first_id),
            focused: first_id,
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
            renaming_tab: None,
            last_ctrl_c: None,
            selection: None,
            last_left_click: None,
            last_wheel_at: None,
            pending_arrow: None,
            pending_mouse: None,
            mouse_local_drag: false,
            context_menu: None,
            branch_cache: HashMap::new(),
        })
    }

    /// ペイン由来の状態 (Claude セッション / 実行状態 / タスク名 / git ブランチ)
    /// をまとめて更新する。イベントループの定期 tick からのみ呼ぶ。描画パスでは
    /// 呼ばない (描画側はここで作ったキャッシュを読むだけ)。
    pub fn refresh_pane_state(&mut self) {
        // 「アクティブタブに属するペインか」の判定用。DoneUnseen (未閲覧完了)
        // はアクティブタブでは発生させない。
        let active_leaves: Vec<PaneId> = self
            .tabs
            .get(self.active_tab)
            .map(|t| t.layout.leaves())
            .unwrap_or_default();
        for pane in self.panes.values_mut() {
            pane.session.refresh();
            if !pane.claude_running {
                // shell ペインに状態色・タスク名は出さない
                // (focused_model_label と同じゲート)。
                pane.status = ClaudeStatus::Idle;
                pane.task = None;
                continue;
            }
            let tab_active = active_leaves.contains(&pane.id);
            let title_task = match pane.parser.lock() {
                Ok(p) => {
                    let screen = p.screen();
                    // スクロールバック閲覧中は contents() が過去の画面を返す
                    // ため判定しない (前回の状態を据え置き、最下部へ戻った
                    // 次の tick で回復する)。
                    if screen.scrollback() == 0 {
                        pane.status = detect_status(&screen.contents(), pane.status, tab_active);
                    }
                    sanitize_task(screen.title())
                }
                Err(_) => None,
            };
            // OSC タイトルが取れればそれを優先、無ければセッション JSONL の
            // 最後のユーザー発話にフォールバック。
            pane.task = title_task.or_else(|| pane.session.info().task.clone());
        }
        // 現存ペインの cwd だけで作り直す = 閉じたペインの分は自然に落ちる。
        let mut next: HashMap<PathBuf, Option<String>> = HashMap::new();
        for cwd in self.panes.values().map(|p| p.cwd.clone()) {
            if next.contains_key(&cwd) {
                continue;
            }
            // 変化していないものは再探索せず前回値を持ち越す…のではなく、
            // ブランチは外部で切り替わるので毎 tick 引き直す。status 走査を
            // しない branch_of なので十分安い。
            let branch = crate::sidebar::git::branch_of(&cwd);
            next.insert(cwd, branch);
        }
        self.branch_cache = next;
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
        self.branch_cache.get(cwd)?.as_deref()
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
        let pane = Pane::spawn(new_id, &cwd, Uuid::new_v4())?;
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
        let pane = Pane::spawn(new_id, &cwd, Uuid::new_v4())?;
        self.panes.insert(new_id, pane);
        self.tabs.push(Tab {
            title: folder_title(&cwd),
            layout: Layout::Leaf(new_id),
            focused: new_id,
        });
        self.active_tab = self.tabs.len() - 1;
        Ok(())
    }

    pub fn close_focused_pane(&mut self) {
        let focused = self.current_tab().focused;
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
                // Tab is empty — remove it.
                self.tabs.remove(self.active_tab);
                if self.tabs.is_empty() {
                    self.quit = true;
                } else if self.active_tab >= self.tabs.len() {
                    self.active_tab = self.tabs.len() - 1;
                }
            }
        }
    }

    pub fn next_tab(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        self.active_tab = (self.active_tab + 1) % self.tabs.len();
        self.mark_active_tab_seen();
    }

    pub fn prev_tab(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        if self.active_tab == 0 {
            self.active_tab = self.tabs.len() - 1;
        } else {
            self.active_tab -= 1;
        }
        self.mark_active_tab_seen();
    }

    /// アクティブタブ配下の DoneUnseen (未閲覧完了) を即座に Idle へ戻す。
    /// タブ切替直後にマゼンタを 2 秒 tick を待たず消すための補助で、
    /// 呼び忘れても次 tick の detect_status が同じ結果に収束する。
    pub fn mark_active_tab_seen(&mut self) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        for pid in tab.layout.leaves() {
            if let Some(p) = self.panes.get_mut(&pid) {
                if p.status == ClaudeStatus::DoneUnseen {
                    p.status = ClaudeStatus::Idle;
                }
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
