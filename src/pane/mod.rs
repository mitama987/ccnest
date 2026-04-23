pub mod grid;
pub mod pty;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use uuid::Uuid;

use crate::claude::launcher::{spawn_claude, spawn_shell};

pub type PaneId = u64;

pub struct Pane {
    pub id: PaneId,
    pub cwd: PathBuf,
    pub session_id: Uuid,
    pub created_at: Instant,
    pub pty: pty::PtyHandle,
    pub parser: Arc<Mutex<vt100::Parser>>,
    pub command: String,
    pub claude_running: bool,
}

impl Pane {
    pub fn spawn(id: PaneId, cwd: &Path, session_id: Uuid) -> Result<Self> {
        let parser = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 2000)));
        let (pty, command, claude_running) = spawn_claude(cwd, session_id, Arc::clone(&parser))?;
        Ok(Self {
            id,
            cwd: cwd.to_path_buf(),
            session_id,
            created_at: Instant::now(),
            pty,
            parser,
            command,
            claude_running,
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
}
