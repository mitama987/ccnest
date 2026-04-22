pub mod grid;
pub mod pty;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use uuid::Uuid;

use crate::claude::launcher::spawn_claude;

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
}
