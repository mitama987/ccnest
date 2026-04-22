use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use portable_pty::CommandBuilder;
use uuid::Uuid;

use crate::pane::pty::PtyHandle;

/// Spawn a `claude` process in a PTY, with a fallback to the system shell
/// when `claude` is not available. Returns (handle, command-used, claude-running?).
pub fn spawn_claude(
    cwd: &Path,
    session_id: Uuid,
    parser: Arc<Mutex<vt100::Parser>>,
) -> Result<(PtyHandle, String, bool)> {
    // First attempt: `claude --session-id <uuid>`.
    let mut cmd = CommandBuilder::new(claude_bin());
    cmd.arg("--session-id");
    cmd.arg(session_id.to_string());
    cmd.cwd(cwd);
    for (k, v) in std::env::vars() {
        cmd.env(k, v);
    }

    match PtyHandle::spawn(cmd, Arc::clone(&parser)) {
        Ok(h) => Ok((h, "claude".to_string(), true)),
        Err(_) => {
            // Fallback: just start the system shell so the pane is usable.
            let shell = if cfg!(windows) {
                std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_string())
            } else {
                std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
            };
            let mut fallback = CommandBuilder::new(shell.clone());
            fallback.cwd(cwd);
            for (k, v) in std::env::vars() {
                fallback.env(k, v);
            }
            let h = PtyHandle::spawn(fallback, parser)?;
            Ok((h, shell, false))
        }
    }
}

fn claude_bin() -> String {
    // Allow override for tests / custom installs.
    if let Ok(bin) = std::env::var("CCNEST_CLAUDE_BIN") {
        return bin;
    }
    if cfg!(windows) {
        "claude.cmd"
    } else {
        "claude"
    }
    .to_string()
}
