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
}

/// ペイン内のテキスト選択範囲。anchor/cursor はペイン内コンテンツ座標 (col,row)。
#[derive(Debug, Clone, Copy)]
pub struct Selection {
    pub pane_id: PaneId,
    pub anchor: (u16, u16),
    pub cursor: (u16, u16),
    /// マウスボタン押下中。false になっても選択は残し続け、Ctrl+C か
    /// 新規クリックでクリアされる。
    pub dragging: bool,
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
