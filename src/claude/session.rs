use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Deserialize;

/// Default context window for the Claude models we care about in v0.1.
/// Overridden by env var `CCNEST_CONTEXT_WINDOW` if set.
pub const DEFAULT_CONTEXT_WINDOW: u64 = 200_000;

#[derive(Debug, Clone, Copy)]
pub struct ContextUsage {
    pub used: u64,
    pub window: u64,
}

impl ContextUsage {
    pub fn remaining(&self) -> u64 {
        self.window.saturating_sub(self.used)
    }
    pub fn ratio(&self) -> f32 {
        if self.window == 0 {
            0.0
        } else {
            (self.used as f32 / self.window as f32).min(1.0)
        }
    }
}

#[derive(Debug, Deserialize)]
struct Entry {
    #[serde(default)]
    message: Option<Message>,
}

#[derive(Debug, Deserialize)]
struct Message {
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize, Default)]
struct Usage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
}

/// Translate a filesystem cwd into the encoded directory name that Claude Code
/// uses under `~/.claude/projects/`.
///
/// Rule: drop the drive colon (Windows), then replace `\` and `/` with `-`.
pub fn encode_project_dir(cwd: &Path) -> String {
    let s = cwd.to_string_lossy();
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' | '/' => out.push('-'),
            ':' => {} // drop drive colon
            other => out.push(other),
        }
    }
    out
}

pub fn claude_projects_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("projects"))
}

pub fn session_path(cwd: &Path, session_id: &str) -> Option<PathBuf> {
    claude_projects_root().map(|root| {
        root.join(encode_project_dir(cwd))
            .join(format!("{session_id}.jsonl"))
    })
}

/// Parse a JSONL file and compute current context usage.
/// We treat the *last* assistant message's usage as authoritative (Claude
/// reports cumulative input including cache) — this is how ccusage and
/// similar tools handle it.
pub fn usage_from_file(path: &Path) -> Result<ContextUsage> {
    let content = fs::read_to_string(path)?;
    usage_from_str(&content)
}

pub fn usage_from_str(content: &str) -> Result<ContextUsage> {
    let mut last_usage: Option<Usage> = None;
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Entry>(line) else {
            continue;
        };
        if let Some(u) = entry.message.and_then(|m| m.usage) {
            last_usage = Some(u);
        }
    }
    let used = last_usage
        .map(|u| {
            u.input_tokens.unwrap_or(0)
                + u.cache_creation_input_tokens.unwrap_or(0)
                + u.cache_read_input_tokens.unwrap_or(0)
                + u.output_tokens.unwrap_or(0)
        })
        .unwrap_or(0);
    let window = std::env::var("CCNEST_CONTEXT_WINDOW")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_CONTEXT_WINDOW);
    Ok(ContextUsage { used, window })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_windows_path() {
        let p = PathBuf::from(r"C:\Users\me\proj");
        assert_eq!(encode_project_dir(&p), "C-Users-me-proj");
    }

    #[test]
    fn encodes_unix_path() {
        let p = PathBuf::from("/home/me/proj");
        assert_eq!(encode_project_dir(&p), "-home-me-proj");
    }

    #[test]
    fn empty_jsonl_gives_zero_usage() {
        let u = usage_from_str("").unwrap();
        assert_eq!(u.used, 0);
    }

    #[test]
    fn usage_takes_last_message() {
        let jsonl = r#"
{"type":"user","message":{"role":"user"}}
{"type":"assistant","message":{"role":"assistant","usage":{"input_tokens":1000,"cache_read_input_tokens":500,"output_tokens":200}}}
{"type":"assistant","message":{"role":"assistant","usage":{"input_tokens":1500,"cache_read_input_tokens":800,"output_tokens":300}}}
"#;
        let u = usage_from_str(jsonl).unwrap();
        assert_eq!(u.used, 1500 + 800 + 300);
    }

    #[test]
    fn tolerates_malformed_lines() {
        let jsonl = "garbage\n{\"message\":{\"usage\":{\"input_tokens\":42}}}\n";
        let u = usage_from_str(jsonl).unwrap();
        assert_eq!(u.used, 42);
    }
}
