use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;
use uuid::Uuid;

use crate::pane::grid::{Direction, Layout};
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
    /// 直近のマウスホイールイベント時刻。Windows ConPTY がホイール回転を Mouse
    /// イベントと plain な Up/Down KeyEvent の両方として配信するケースで、
    /// 後者を「ファントム矢印キー」として握りつぶすために使う。process_batch
    /// 内で wheel を見たら更新し、handle_key 直前の判定で参照する。
    pub last_wheel_at: Option<Instant>,
    /// ホイール由来のファントム Up/Down を相殺するための予算。wheel イベント
    /// ごとに飽和加算し、plain Up/Down を 1 つ握りつぶすたびに 1 消費する。
    /// タイミング窓ではなく予算 + バッチ内隣接で判定するため、イベントループが
    /// 重くて wheel とファントム矢印が別バッチに分離してもファントムを取り
    /// 逃さない。`last_wheel_at` からの経過が decay を超えたら 0 に戻す。
    pub wheel_budget: u8,
    /// マウスモード ON のペインで左ボタンを押した直後の保留状態。
    /// 「クリック (= アプリへ転送)」か「ドラッグ (= ccnest ローカル選択)」かを
    /// 次イベントで判定するまで Down を転送せずここに溜める。
    pub pending_mouse: Option<PendingMouse>,
    /// マウスモード ON のペインで素ドラッグによる ccnest ローカル選択が進行中。
    /// この間 Drag/Up は転送せず既存ローカル選択アームへ流す。
    pub mouse_local_drag: bool,
}

/// 左 Down 押下の保留 (press/drag 判定待ち)。座標はペインローカル (0 始まり)。
#[derive(Debug, Clone, Copy)]
pub struct PendingMouse {
    pub pid: PaneId,
    pub lx: u16,
    pub ly: u16,
    pub at: Instant,
}

/// ペイン内のテキスト選択範囲。anchor/cursor はペイン内コンテンツ座標 (col,row)。
///
/// row は `i32` で持ち、負値は「現在の表示範囲よりさらに上 (スクロールバック内)」
/// を表す。これによりドラッグ中の auto-scroll で表示外に出た anchor も追跡できる。
#[derive(Debug, Clone, Copy)]
pub struct Selection {
    pub pane_id: PaneId,
    pub anchor: (u16, i32),
    pub cursor: (u16, i32),
    /// マウスボタン押下中。false になっても選択は残し続け、Ctrl+C か
    /// 新規クリックでクリアされる。
    pub dragging: bool,
    /// drag 中にペイン端を超えたときの auto-scroll 方向。
    /// `-1` = 上端外 (古い行へ), `+1` = 下端外 (新しい行へ), `0` = ペイン内。
    /// イベントループの tick で参照され、1 行/tick で scroll_by に反映される。
    pub auto_scroll: i32,
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
            wheel_budget: 0,
            pending_mouse: None,
            mouse_local_drag: false,
        })
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
