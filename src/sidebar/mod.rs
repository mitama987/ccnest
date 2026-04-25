pub mod claude_ctx;
pub mod filetree;
pub mod git;
pub mod panelist;

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    FileTree = 0,
    Claude = 1,
    Git = 2,
    Panes = 3,
}

impl Section {
    pub fn all() -> [Section; 4] {
        [
            Section::FileTree,
            Section::Claude,
            Section::Git,
            Section::Panes,
        ]
    }
    pub fn title(self) -> &'static str {
        match self {
            Section::FileTree => "Files",
            Section::Claude => "Claude",
            Section::Git => "Git",
            Section::Panes => "Panes",
        }
    }
}

pub struct SidebarState {
    pub visible: bool,
    pub active: Section,
    pub cursors: [usize; 4],
    pub cwd: PathBuf,
    pub file_tree: filetree::FileTree,
    pub git_info: Option<git::GitInfo>,
}

impl SidebarState {
    pub fn new(cwd: PathBuf) -> Self {
        let file_tree = filetree::FileTree::new(cwd.clone());
        let git_info = git::load(&cwd).ok();
        Self {
            visible: false,
            active: Section::FileTree,
            cursors: [0; 4],
            cwd,
            file_tree,
            git_info,
        }
    }

    pub fn refresh(&mut self) {
        self.file_tree.refresh();
        self.git_info = git::load(&self.cwd).ok();
    }

    pub fn cursor(&self) -> usize {
        self.cursors[self.active as usize]
    }

    pub fn set_cursor(&mut self, v: usize) {
        self.cursors[self.active as usize] = v;
    }

    pub fn move_cursor(&mut self, delta: i32, max: usize) {
        if max == 0 {
            self.set_cursor(0);
            return;
        }
        let cur = self.cursor() as i32;
        let next = (cur + delta).clamp(0, (max as i32).saturating_sub(1));
        self.set_cursor(next as usize);
    }

    pub fn cycle_section(&mut self) {
        let idx = self.active as usize;
        let next = (idx + 1) % 4;
        self.active = Section::all()[next];
    }

    pub fn jump_section(&mut self, idx: u8) {
        if (idx as usize) < 4 {
            self.active = Section::all()[idx as usize];
        }
    }
}
