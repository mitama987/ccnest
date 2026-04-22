use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
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
    if let Some(bin) = resolve_claude_bin() {
        let mut cmd = CommandBuilder::new(&bin);
        cmd.arg("--session-id");
        cmd.arg(session_id.to_string());
        cmd.cwd(cwd);
        apply_env(&mut cmd);
        if let Ok(h) = PtyHandle::spawn(cmd, Arc::clone(&parser)) {
            let label = bin
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "claude".to_string());
            return Ok((h, label, true));
        }
    }

    // Fallback: start the system shell so the pane is at least usable.
    let shell = if cfg!(windows) {
        std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_string())
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    };
    let mut fallback = CommandBuilder::new(&shell);
    fallback.cwd(cwd);
    apply_env(&mut fallback);
    let h = PtyHandle::spawn(fallback, parser)
        .map_err(|e| anyhow!("failed to spawn both claude and shell: {e}"))?;
    Ok((h, shell, false))
}

fn apply_env(cmd: &mut CommandBuilder) {
    for (k, v) in std::env::vars() {
        cmd.env(k, v);
    }
    // Keep interactive TUIs happy on ConPTY.
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("FORCE_COLOR", "1");
    cmd.env("CI", "");
}

/// Locate the `claude` executable by walking `PATH` with all plausible
/// Windows extensions. Honors `CCNEST_CLAUDE_BIN` for manual override.
fn resolve_claude_bin() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("CCNEST_CLAUDE_BIN") {
        let p = PathBuf::from(custom);
        if p.is_file() {
            return Some(p);
        }
    }

    let exts: &[&str] = if cfg!(windows) {
        &["exe", "cmd", "bat", "ps1", ""]
    } else {
        &[""]
    };

    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for ext in exts {
            let candidate = if ext.is_empty() {
                dir.join("claude")
            } else {
                dir.join(format!("claude.{ext}"))
            };
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}
