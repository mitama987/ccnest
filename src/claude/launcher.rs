use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use portable_pty::CommandBuilder;
use uuid::Uuid;

use crate::pane::pty::{PtyHandle, ReaderHooks};

/// bypass (パーミッション確認スキップ) 系フラグの渡し方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BypassFlag {
    /// `--dangerously-skip-permissions`: bypass モードで開始する (従来の既定動作)。
    StartIn,
    /// `--allow-dangerously-skip-permissions`: 開始モードは変えずに
    /// Shift+Tab サイクルへ bypassPermissions を追加する (claude v2.1 系で確認)。
    AllowCycle,
    /// bypass 系フラグを一切渡さない。
    Off,
}

/// claude 起動フラグの決定結果。env 読み取りは [`spawn_opts_from`] に隔離し、
/// 引数組み立て ([`build_claude_args`]) は純粋にテストできるようにしている。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnOpts {
    /// `--permission-mode` に渡す値。None ならフラグ自体を渡さない。
    pub permission_mode: Option<String>,
    pub bypass: BypassFlag,
}

/// 環境変数から [`SpawnOpts`] を決定する。lookup を注入する形なので
/// テストからは env を変異させずに全分岐を検証できる。
///
/// - `CCNEST_CLAUDE_PERMISSION_MODE` 未設定/空 → `plan` で開始 (既定)。
///   bypass はサイクル追加のみ (`AllowCycle`) なので、プラン承認後に
///   Shift+Tab で従来どおりの全自動 (bypass) に切り替えられる。
/// - `off` / `bypass` (大文字小文字無視) → 従来動作: `--permission-mode` なしで
///   bypass 開始 (`StartIn`)。
/// - その他の値 → そのまま `--permission-mode` に透過 (将来のモード追加に追従)。
/// - `CCNEST_CLAUDE_NO_SKIP_PERMISSIONS` が設定されていれば bypass 系フラグを
///   一切渡さない (従来からの安全弁)。
pub fn spawn_opts_from(get: impl Fn(&str) -> Option<String>) -> SpawnOpts {
    let no_skip = get("CCNEST_CLAUDE_NO_SKIP_PERMISSIONS").is_some();
    let mode_env = get("CCNEST_CLAUDE_PERMISSION_MODE");
    let permission_mode = match mode_env.as_deref().map(str::trim) {
        None | Some("") => Some("plan".to_string()),
        Some(m) if m.eq_ignore_ascii_case("off") || m.eq_ignore_ascii_case("bypass") => None,
        Some(m) => Some(m.to_string()),
    };
    let bypass = if no_skip {
        BypassFlag::Off
    } else if permission_mode.is_none() {
        BypassFlag::StartIn
    } else {
        BypassFlag::AllowCycle
    };
    SpawnOpts {
        permission_mode,
        bypass,
    }
}

/// `claude` に渡す引数列 (バイナリ名を除く) を組み立てる純粋関数。
pub fn build_claude_args(session_id: &str, opts: &SpawnOpts) -> Vec<String> {
    let mut args = vec!["--session-id".to_string(), session_id.to_string()];
    if let Some(mode) = &opts.permission_mode {
        args.push("--permission-mode".to_string());
        args.push(mode.clone());
    }
    match opts.bypass {
        BypassFlag::StartIn => args.push("--dangerously-skip-permissions".to_string()),
        BypassFlag::AllowCycle => args.push("--allow-dangerously-skip-permissions".to_string()),
        BypassFlag::Off => {}
    }
    args
}

/// Spawn a `claude` process in a PTY, with a fallback to the system shell
/// when `claude` is not available. Returns (handle, command-used, claude-running?).
pub fn spawn_claude(
    cwd: &Path,
    session_id: Uuid,
    parser: Arc<Mutex<vt100::Parser>>,
    hooks: ReaderHooks,
) -> Result<(PtyHandle, String, bool)> {
    // Parallel ccnest panes share ~/.claude.json; a force-quit mid-write can
    // corrupt it. Self-heal right before launching so the new claude starts
    // from a valid file. Best-effort — never block the spawn.
    let _ = crate::claude::config::heal_claude_config();

    if let Some(bin) = resolve_claude_bin() {
        let mut cmd = CommandBuilder::new(&bin);
        // 既定はプランモード開始 + Shift+Tab サイクルに bypass 追加。
        // 従来の bypass 直行に戻すには CCNEST_CLAUDE_PERMISSION_MODE=off。
        // 詳細は spawn_opts_from のドキュメントコメントを参照。
        let opts = spawn_opts_from(|k| std::env::var(k).ok());
        for arg in build_claude_args(&session_id.to_string(), &opts) {
            cmd.arg(arg);
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
        if let Ok(h) = PtyHandle::spawn(cmd, Arc::clone(&parser), hooks.clone()) {
            let label = bin
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "claude".to_string());
            return Ok((h, label, true));
        }
    }

    // Fallback: start the system shell so the pane is at least usable.
    let (h, shell) = spawn_shell(cwd, parser, hooks)
        .map_err(|e| anyhow!("failed to spawn both claude and shell: {e}"))?;
    Ok((h, shell, false))
}

/// Spawn the system shell in a PTY (cmd.exe on Windows, `$SHELL` otherwise).
/// Used both as the initial fallback when `claude` is missing and when Ctrl+C
/// 2 連打でペインを shell に切り戻すときの再起動先として使う。
pub fn spawn_shell(
    cwd: &Path,
    parser: Arc<Mutex<vt100::Parser>>,
    hooks: ReaderHooks,
) -> Result<(PtyHandle, String)> {
    let shell = if cfg!(windows) {
        std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_string())
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    };
    let mut cmd = CommandBuilder::new(&shell);
    cmd.cwd(cwd);
    apply_env(&mut cmd);
    let h =
        PtyHandle::spawn(cmd, parser, hooks).map_err(|e| anyhow!("failed to spawn shell: {e}"))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn env_none(_: &str) -> Option<String> {
        None
    }

    // 既定 (env 無し): プランモード開始 + bypass はサイクル追加のみ
    #[test]
    fn default_opts_are_plan_with_allow_cycle() {
        let opts = spawn_opts_from(env_none);
        assert_eq!(
            opts,
            SpawnOpts {
                permission_mode: Some("plan".to_string()),
                bypass: BypassFlag::AllowCycle,
            }
        );
    }

    // 既定の引数列: session-id が先頭、plan + allow-cycle が続く
    #[test]
    fn default_args_contain_plan_and_allow_flag() {
        let args = build_claude_args("abc", &spawn_opts_from(env_none));
        assert_eq!(
            args,
            [
                "--session-id",
                "abc",
                "--permission-mode",
                "plan",
                "--allow-dangerously-skip-permissions",
            ]
        );
    }

    // PERMISSION_MODE=off で従来動作 (bypass 開始・--permission-mode なし)
    #[test]
    fn mode_off_restores_legacy_bypass_start() {
        let get = |k: &str| (k == "CCNEST_CLAUDE_PERMISSION_MODE").then(|| "off".to_string());
        let args = build_claude_args("abc", &spawn_opts_from(get));
        assert_eq!(
            args,
            ["--session-id", "abc", "--dangerously-skip-permissions"]
        );
    }

    // "Bypass" も大文字小文字無視で従来動作
    #[test]
    fn mode_bypass_case_insensitive_restores_legacy() {
        let get = |k: &str| (k == "CCNEST_CLAUDE_PERMISSION_MODE").then(|| "Bypass".to_string());
        assert_eq!(spawn_opts_from(get).bypass, BypassFlag::StartIn);
    }

    // NO_SKIP_PERMISSIONS 設定時は bypass 系フラグ皆無 (プランモードは維持)
    #[test]
    fn no_skip_env_disables_bypass_flags_keeps_plan() {
        let get = |k: &str| (k == "CCNEST_CLAUDE_NO_SKIP_PERMISSIONS").then(|| "1".to_string());
        let args = build_claude_args("abc", &spawn_opts_from(get));
        assert_eq!(args, ["--session-id", "abc", "--permission-mode", "plan"]);
    }

    // NO_SKIP + off の併用では session-id のみ (完全な素の claude)
    #[test]
    fn no_skip_plus_off_yields_session_id_only() {
        let get = |k: &str| match k {
            "CCNEST_CLAUDE_NO_SKIP_PERMISSIONS" => Some("1".to_string()),
            "CCNEST_CLAUDE_PERMISSION_MODE" => Some("off".to_string()),
            _ => None,
        };
        let args = build_claude_args("abc", &spawn_opts_from(get));
        assert_eq!(args, ["--session-id", "abc"]);
    }

    // plan/off 以外の値は --permission-mode にそのまま透過
    #[test]
    fn custom_mode_passes_through() {
        let get =
            |k: &str| (k == "CCNEST_CLAUDE_PERMISSION_MODE").then(|| "acceptEdits".to_string());
        let args = build_claude_args("abc", &spawn_opts_from(get));
        assert_eq!(
            args,
            [
                "--session-id",
                "abc",
                "--permission-mode",
                "acceptEdits",
                "--allow-dangerously-skip-permissions",
            ]
        );
    }

    // 空文字の PERMISSION_MODE は未設定と同じく既定 (plan) 扱い
    #[test]
    fn empty_mode_env_means_default_plan() {
        let get = |k: &str| (k == "CCNEST_CLAUDE_PERMISSION_MODE").then(String::new);
        assert_eq!(
            spawn_opts_from(get).permission_mode.as_deref(),
            Some("plan")
        );
    }
}

// Version History
// - ver1.1 (2026-08-11): 起動フラグを SpawnOpts + build_claude_args に純関数化。
//   既定を「--permission-mode plan + --allow-dangerously-skip-permissions」
//   (プランモード開始・承認後 Shift+Tab で bypass 可) に変更。
//   CCNEST_CLAUDE_PERMISSION_MODE=off で従来の bypass 直行に戻せる。
// - ver1.0: 初版 (--session-id + --dangerously-skip-permissions 直書き)
