//! Self-heal for `~/.claude.json`.
//!
//! `~/.claude.json` is Claude Code's shared global config. Every ccnest pane is
//! its own Claude Code process, so several of them write that one file in
//! parallel. When they are killed mid-write — e.g. force-quitting ccnest — the
//! file can be left with trailing garbage (a stray `}`), and the next launch
//! fails with "invalid JSON".
//!
//! We can't change how Claude Code writes the file, so we make the corruption
//! self-correcting instead: right before ccnest spawns a `claude` process we
//! validate the file, snapshot it to `~/.claude-backups/` while it is healthy,
//! and restore the newest healthy snapshot if it has gone bad. Everything here
//! is best-effort — any error is logged and swallowed so a spawn never fails
//! because of it.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

/// Number of healthy snapshots to keep in the backup directory.
const KEEP_BACKUPS: usize = 5;

/// Validate `~/.claude.json`; restore the newest healthy backup if it is
/// corrupt, or snapshot it for future recovery if it is healthy. Best-effort.
pub fn heal_claude_config() -> Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("could not resolve home directory"))?;
    heal_at(&home.join(".claude.json"), &home.join(".claude-backups"))
}

/// Core logic, parameterised on paths so it can be unit-tested without touching
/// the real home directory.
fn heal_at(config: &Path, backups: &Path) -> Result<()> {
    // No config yet (fresh install) — nothing to protect.
    if !config.exists() {
        return Ok(());
    }

    let raw = fs::read_to_string(config)?;
    if is_valid_json(&raw) {
        snapshot(config, backups)?;
    } else {
        restore(config, backups)?;
    }
    Ok(())
}

fn is_valid_json(s: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(s).is_ok()
}

/// Copy the (healthy) config into the backup dir under a timestamped name, then
/// prune all but the newest `KEEP_BACKUPS` snapshots.
fn snapshot(config: &Path, backups: &Path) -> Result<()> {
    fs::create_dir_all(backups)?;
    // Fixed-width, lexically sortable timestamp (see `backups_newest_first`).
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S%.3f");
    fs::copy(config, backups.join(format!("claude-{stamp}.json")))?;
    prune(backups)?;
    Ok(())
}

/// Restore the newest backup that still parses as valid JSON over the corrupt
/// config. If no valid backup exists, leave the file alone and just warn.
fn restore(config: &Path, backups: &Path) -> Result<()> {
    for backup in backups_newest_first(backups)? {
        if let Ok(raw) = fs::read_to_string(&backup) {
            if is_valid_json(&raw) {
                fs::copy(&backup, config)?;
                eprintln!(
                    "ccnest: ~/.claude.json was corrupt — restored from {}",
                    backup.display()
                );
                return Ok(());
            }
        }
    }
    eprintln!("ccnest: ~/.claude.json is corrupt and no valid backup was found");
    Ok(())
}

/// Backup files (`claude-*.json`) sorted newest first.
///
/// The names embed a fixed-width timestamp, so a reverse lexical sort on the
/// file name is newest-first and deterministic — no reliance on filesystem
/// mtime, which `fs::copy` does not preserve reliably.
fn backups_newest_first(backups: &Path) -> Result<Vec<PathBuf>> {
    if !backups.is_dir() {
        return Ok(Vec::new());
    }
    let mut files: Vec<PathBuf> = fs::read_dir(backups)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| is_backup_file(p))
        .collect();
    files.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    Ok(files)
}

fn is_backup_file(p: &Path) -> bool {
    p.file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|n| n.starts_with("claude-") && n.ends_with(".json"))
}

/// Delete every snapshot beyond the newest `KEEP_BACKUPS`.
fn prune(backups: &Path) -> Result<()> {
    for old in backups_newest_first(backups)?
        .into_iter()
        .skip(KEEP_BACKUPS)
    {
        let _ = fs::remove_file(old);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, s: &str) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, s).unwrap();
    }

    #[test]
    fn missing_config_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join(".claude.json");
        let backups = dir.path().join(".claude-backups");
        assert!(heal_at(&config, &backups).is_ok());
        assert!(!backups.exists());
    }

    #[test]
    fn valid_config_is_snapshotted() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join(".claude.json");
        let backups = dir.path().join(".claude-backups");
        write(&config, r#"{"numStartups":1}"#);

        heal_at(&config, &backups).unwrap();

        let snaps = backups_newest_first(&backups).unwrap();
        assert_eq!(snaps.len(), 1);
        assert_eq!(
            fs::read_to_string(&snaps[0]).unwrap(),
            r#"{"numStartups":1}"#
        );
        // Config itself is untouched.
        assert_eq!(fs::read_to_string(&config).unwrap(), r#"{"numStartups":1}"#);
    }

    #[test]
    fn corrupt_config_is_restored_from_newest_valid_backup() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join(".claude.json");
        let backups = dir.path().join(".claude-backups");

        write(
            &backups.join("claude-20260101-000000.000.json"),
            r#"{"v":1}"#,
        );
        write(
            &backups.join("claude-20260102-000000.000.json"),
            r#"{"v":2}"#,
        );
        // The real-world failure: a valid object with one stray trailing brace.
        write(&config, r#"{"v":3}}"#);

        heal_at(&config, &backups).unwrap();

        assert_eq!(fs::read_to_string(&config).unwrap(), r#"{"v":2}"#);
    }

    #[test]
    fn restore_skips_corrupt_backups() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join(".claude.json");
        let backups = dir.path().join(".claude-backups");

        write(
            &backups.join("claude-20260101-000000.000.json"),
            r#"{"v":1}"#,
        );
        // Newest backup is itself corrupt — must be skipped.
        write(
            &backups.join("claude-20260103-000000.000.json"),
            r#"{"v":3}}"#,
        );
        write(&config, "not json {");

        heal_at(&config, &backups).unwrap();

        assert_eq!(fs::read_to_string(&config).unwrap(), r#"{"v":1}"#);
    }

    #[test]
    fn prune_keeps_only_newest() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join(".claude-backups");
        for i in 1..=(KEEP_BACKUPS + 3) {
            write(
                &backups.join(format!("claude-202601{i:02}-000000.000.json")),
                "{}",
            );
        }

        prune(&backups).unwrap();

        let remaining = backups_newest_first(&backups).unwrap();
        assert_eq!(remaining.len(), KEEP_BACKUPS);
        // The newest (highest day number) must survive.
        assert_eq!(
            remaining[0].file_name().unwrap().to_str().unwrap(),
            format!("claude-202601{:02}-000000.000.json", KEEP_BACKUPS + 3)
        );
    }
}
