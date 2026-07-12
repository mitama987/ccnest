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
    // Parallel ccnest panes share ~/.claude.json; a force-quit mid-write can
    // corrupt it. Self-heal right before launching so the new claude starts
    // from a valid file. Best-effort — never block the spawn.
    let _ = crate::claude::config::heal_claude_config();

    if let Some(bin) = resolve_claude_bin() {
        let mut cmd = CommandBuilder::new(&bin);
        cmd.arg("--session-id");
        cmd.arg(session_id.to_string());
        // ccnest からは毎回パーミッション確認なしで Claude を起動するのが既定動作。
        // 将来公開する場合などに備え、`CCNEST_CLAUDE_NO_SKIP_PERMISSIONS=1` を設定
        // すると opt-out できる安全弁を残してある。
        if std::env::var_os("CCNEST_CLAUDE_NO_SKIP_PERMISSIONS").is_none() {
            cmd.arg("--dangerously-skip-permissions");
        }
        cmd.cwd(cwd);
        apply_env(&mut cmd);
        // fullscreen (alt 画面) の Claude Code は自前で画面を描き直すため、
        // ccnest のローカル選択 (反転) がスクロールに追従できない。ccnest 内では
        // classic モード (ネイティブスクロールバック描画) で起動し、選択・ホイール
        // スクロールをシェルペインと同じ vt100 scrollback ベースに揃える。
        // settings.json の "tui": "fullscreen" よりこの env が優先される。
        // 旧挙動に戻したい場合は CCNEST_CLAUDE_ALT_SCREEN=1 で opt-out。
        if std::env::var_os("CCNEST_CLAUDE_ALT_SCREEN").is_none() {
            cmd.env("CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN", "1");
        }
        if let Ok(h) = PtyHandle::spawn(cmd, Arc::clone(&parser)) {
            let label = bin
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "claude".to_string());
            return Ok((h, label, true));
        }
    }

    // Fallback: start the system shell so the pane is at least usable.
    let (h, shell) = spawn_shell(cwd, parser)
        .map_err(|e| anyhow!("failed to spawn both claude and shell: {e}"))?;
    Ok((h, shell, false))
}

/// Spawn the system shell in a PTY (cmd.exe on Windows, `$SHELL` otherwise).
/// Used both as the initial fallback when `claude` is missing and when Ctrl+C
/// 2 連打でペインを shell に切り戻すときの再起動先として使う。
pub fn spawn_shell(cwd: &Path, parser: Arc<Mutex<vt100::Parser>>) -> Result<(PtyHandle, String)> {
    let shell = if cfg!(windows) {
        std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_string())
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    };
    let mut cmd = CommandBuilder::new(&shell);
    cmd.cwd(cwd);
    apply_env(&mut cmd);
    let h = PtyHandle::spawn(cmd, parser).map_err(|e| anyhow!("failed to spawn shell: {e}"))?;
    Ok((h, shell))
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
