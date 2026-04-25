use std::path::{Path, PathBuf};

use anyhow::Result;
use ignore::WalkBuilder;

#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub depth: usize,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Directory,
    Git,
    Markdown,
    Image,
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Web,
    Json,
    Config,
    Shell,
    Lock,
    Dotfile,
    Text,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayParts {
    pub indent: String,
    pub icon: &'static str,
    pub name: String,
    pub kind: EntryKind,
}

impl Entry {
    pub fn display_parts(&self, root: &Path) -> DisplayParts {
        let name = self.name(root);
        let kind = self.kind();

        DisplayParts {
            indent: "  ".repeat(self.depth.saturating_sub(1)),
            icon: icon_for_kind(kind),
            name: if self.is_dir {
                format!("{name}/")
            } else {
                name
            },
            kind,
        }
    }

    pub fn display(&self, root: &Path) -> String {
        let parts = self.display_parts(root);
        format!("{}{} {}", parts.indent, parts.icon, parts.name)
    }

    pub fn kind(&self) -> EntryKind {
        classify_path(&self.path, self.is_dir)
    }

    fn name(&self, root: &Path) -> String {
        let rel = self.path.strip_prefix(root).unwrap_or(&self.path);
        rel.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| rel.to_string_lossy().to_string())
    }
}

pub fn icon_for_kind(kind: EntryKind) -> &'static str {
    // Nerd Font PUA グリフだと未インストール環境で菱形にフォールバックするため、
    // Windows Terminal が Segoe UI Emoji 経由で確実に描ける emoji を使う。
    match kind {
        EntryKind::Directory => "📁",
        EntryKind::Git => "🌱",
        EntryKind::Markdown => "📝",
        EntryKind::Image => "🖼",
        EntryKind::Rust => "🦀",
        EntryKind::Python => "🐍",
        EntryKind::JavaScript => "📜",
        EntryKind::TypeScript => "📘",
        EntryKind::Web => "🌐",
        EntryKind::Json => "📋",
        EntryKind::Config => "⚙",
        EntryKind::Shell => "🐚",
        EntryKind::Lock => "🔒",
        EntryKind::Dotfile => "•",
        EntryKind::Text => "📃",
        EntryKind::Other => "📄",
    }
}

fn classify_path(path: &Path, is_dir: bool) -> EntryKind {
    if is_dir {
        return EntryKind::Directory;
    }

    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    match name.as_str() {
        ".gitignore" | ".gitattributes" | ".gitmodules" => return EntryKind::Git,
        "cargo.lock" | "package-lock.json" | "pnpm-lock.yaml" | "yarn.lock" | "uv.lock" => {
            return EntryKind::Lock
        }
        _ => {}
    }

    let extension = path
        .extension()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    match extension.as_str() {
        "md" | "mdx" => EntryKind::Markdown,
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico" => EntryKind::Image,
        "rs" => EntryKind::Rust,
        "py" | "pyw" => EntryKind::Python,
        "js" | "mjs" | "cjs" | "jsx" => EntryKind::JavaScript,
        "ts" | "tsx" => EntryKind::TypeScript,
        "html" | "htm" | "css" | "scss" | "sass" => EntryKind::Web,
        "json" | "jsonc" => EntryKind::Json,
        "toml" | "yaml" | "yml" | "ini" | "conf" | "config" | "env" => EntryKind::Config,
        "ps1" | "sh" | "bash" | "bat" | "cmd" => EntryKind::Shell,
        "lock" => EntryKind::Lock,
        "txt" | "log" => EntryKind::Text,
        _ if name.starts_with('.') => EntryKind::Dotfile,
        _ => EntryKind::Other,
    }
}

pub fn walk(root: &Path, max_depth: usize) -> Result<Vec<Entry>> {
    let mut out = Vec::new();
    let walker = WalkBuilder::new(root)
        .max_depth(Some(max_depth))
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .build();
    for dent in walker.flatten() {
        if dent.depth() == 0 {
            continue;
        }
        let is_dir = dent.file_type().map(|t| t.is_dir()).unwrap_or(false);
        out.push(Entry {
            path: dent.path().to_path_buf(),
            depth: dent.depth(),
            is_dir,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(root: &Path, relative: &str, is_dir: bool, depth: usize) -> Entry {
        Entry {
            path: root.join(relative),
            depth,
            is_dir,
        }
    }

    #[test]
    fn display_parts_include_icon_indent_and_directory_suffix() {
        let root = Path::new("workspace");
        let parts = entry(root, "src", true, 1).display_parts(root);

        assert_eq!(parts.kind, EntryKind::Directory);
        assert_eq!(parts.icon, "📁");
        assert_eq!(parts.indent, "");
        assert_eq!(parts.name, "src/");
    }

    #[test]
    fn display_parts_indent_nested_files() {
        let root = Path::new("workspace");
        let parts = entry(root, "src/main.rs", false, 2).display_parts(root);

        assert_eq!(parts.kind, EntryKind::Rust);
        assert_eq!(parts.icon, "🦀");
        assert_eq!(parts.indent, "  ");
        assert_eq!(parts.name, "main.rs");
    }

    #[test]
    fn classifies_common_file_kinds_for_colored_sidebar_rows() {
        let root = Path::new("workspace");
        let cases = [
            (".gitignore", EntryKind::Git, "🌱"),
            (".marprc.yml", EntryKind::Config, "⚙"),
            ("README.md", EntryKind::Markdown, "📝"),
            ("image.png", EntryKind::Image, "🖼"),
            ("script.py", EntryKind::Python, "🐍"),
            ("app.tsx", EntryKind::TypeScript, "📘"),
            ("Cargo.lock", EntryKind::Lock, "🔒"),
            (".cursorignore", EntryKind::Dotfile, "•"),
        ];

        for (relative, expected_kind, expected_icon) in cases {
            let parts = entry(root, relative, false, 1).display_parts(root);
            assert_eq!(parts.kind, expected_kind, "{relative}");
            assert_eq!(parts.icon, expected_icon, "{relative}");
        }
    }
}

// Version History
// ver0.1 - 2026-04-25 - Added file tree icon and kind metadata with classification tests.
