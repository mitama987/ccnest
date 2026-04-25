use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use ignore::WalkBuilder;

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

#[derive(Debug, Clone)]
pub struct FileNode {
    pub path: PathBuf,
    pub name: String,
    pub kind: EntryKind,
    pub is_dir: bool,
    pub expanded: bool,
    /// 子ノードは初回展開時に lazy で読み込む。`None` = 未読み込み、
    /// `Some(empty)` = 読み込み済みで子なし。
    pub children: Option<Vec<FileNode>>,
}

/// アクション結果。Enter / クリック時に呼び出し側が分岐するために返す。
#[derive(Debug, Clone)]
pub enum ActivateResult {
    File(PathBuf),
    DirToggled,
}

#[derive(Debug)]
pub struct FileTree {
    pub root: PathBuf,
    pub entries: Vec<FileNode>,
}

impl FileTree {
    pub fn new(root: PathBuf) -> Self {
        let entries = load_dir(&root).unwrap_or_default();
        Self { root, entries }
    }

    /// 展開済みパスを保持しつつルートを再ロード。
    pub fn refresh(&mut self) {
        let expanded = self.snapshot_expanded();
        self.entries = load_dir(&self.root).unwrap_or_default();
        for e in self.entries.iter_mut() {
            reapply_expansion(e, &expanded);
        }
    }

    /// 表示中のノードを (depth, node) で列挙。
    pub fn flatten(&self) -> Vec<(usize, &FileNode)> {
        let mut out = Vec::new();
        for e in &self.entries {
            collect_visible(e, 0, &mut out);
        }
        out
    }

    pub fn visible_len(&self) -> usize {
        let mut n = 0;
        for e in &self.entries {
            count_visible(e, &mut n);
        }
        n
    }

    /// 表示順 index に対応するノードを「アクティベート」する。
    /// ディレクトリならトグル(必要なら子を lazy ロード)、ファイルならパスを返す。
    pub fn activate_at(&mut self, index: usize) -> Option<ActivateResult> {
        let mut counter = 0usize;
        for e in self.entries.iter_mut() {
            if let Some(action) = activate_node(e, index, &mut counter) {
                return Some(action);
            }
        }
        None
    }

    fn snapshot_expanded(&self) -> HashSet<PathBuf> {
        let mut set = HashSet::new();
        for e in &self.entries {
            collect_expanded_paths(e, &mut set);
        }
        set
    }
}

fn collect_visible<'a>(node: &'a FileNode, depth: usize, out: &mut Vec<(usize, &'a FileNode)>) {
    out.push((depth, node));
    if node.is_dir && node.expanded {
        if let Some(children) = node.children.as_ref() {
            for c in children {
                collect_visible(c, depth + 1, out);
            }
        }
    }
}

fn count_visible(node: &FileNode, out: &mut usize) {
    *out += 1;
    if node.is_dir && node.expanded {
        if let Some(children) = node.children.as_ref() {
            for c in children {
                count_visible(c, out);
            }
        }
    }
}

fn activate_node(
    node: &mut FileNode,
    target: usize,
    counter: &mut usize,
) -> Option<ActivateResult> {
    if *counter == target {
        return Some(if node.is_dir {
            if node.children.is_none() {
                node.children = Some(load_dir(&node.path).unwrap_or_default());
            }
            node.expanded = !node.expanded;
            ActivateResult::DirToggled
        } else {
            ActivateResult::File(node.path.clone())
        });
    }
    *counter += 1;
    if node.is_dir && node.expanded {
        if let Some(children) = node.children.as_mut() {
            for c in children {
                if let Some(r) = activate_node(c, target, counter) {
                    return Some(r);
                }
            }
        }
    }
    None
}

fn collect_expanded_paths(node: &FileNode, out: &mut HashSet<PathBuf>) {
    if node.is_dir && node.expanded {
        out.insert(node.path.clone());
    }
    if let Some(children) = node.children.as_ref() {
        for c in children {
            collect_expanded_paths(c, out);
        }
    }
}

fn reapply_expansion(node: &mut FileNode, expanded: &HashSet<PathBuf>) {
    if node.is_dir && expanded.contains(&node.path) {
        if node.children.is_none() {
            node.children = Some(load_dir(&node.path).unwrap_or_default());
        }
        node.expanded = true;
        if let Some(children) = node.children.as_mut() {
            for c in children {
                reapply_expansion(c, expanded);
            }
        }
    }
}

/// ディレクトリ直下を 1 レベルだけ読み込み、エクスプローラ式に
/// 「フォルダ→ファイル」「同種は名前昇順(大小無視)」で整列する。
fn load_dir(path: &Path) -> Result<Vec<FileNode>> {
    let mut entries: Vec<FileNode> = WalkBuilder::new(path)
        .max_depth(Some(1))
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .build()
        .flatten()
        .filter(|d| d.depth() == 1)
        .filter_map(|d| {
            let p = d.path().to_path_buf();
            let is_dir = d.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let name = p
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if name.is_empty() {
                return None;
            }
            let kind = classify_path(&p, is_dir);
            Some(FileNode {
                path: p,
                name,
                kind,
                is_dir,
                expanded: false,
                children: None,
            })
        })
        .collect();
    entries.sort_by(compare_explorer);
    Ok(entries)
}

fn compare_explorer(a: &FileNode, b: &FileNode) -> Ordering {
    match (a.is_dir, b.is_dir) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn mk_tree(td: &TempDir) -> PathBuf {
        let root = td.path().to_path_buf();
        fs::create_dir(root.join("src")).unwrap();
        fs::create_dir(root.join("docs")).unwrap();
        fs::write(root.join("src/main.rs"), "").unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap();
        fs::write(root.join("README.md"), "").unwrap();
        fs::write(root.join("Cargo.toml"), "").unwrap();
        root
    }

    #[test]
    fn explorer_sort_puts_directories_before_files() {
        let td = TempDir::new().unwrap();
        let root = mk_tree(&td);
        let tree = FileTree::new(root);
        let names: Vec<&str> = tree.entries.iter().map(|e| e.name.as_str()).collect();
        // dirs 昇順 → files 昇順
        assert_eq!(names, vec!["docs", "src", "Cargo.toml", "README.md"]);
    }

    #[test]
    fn folders_collapsed_by_default_show_only_top_level() {
        let td = TempDir::new().unwrap();
        let root = mk_tree(&td);
        let tree = FileTree::new(root);
        assert_eq!(tree.visible_len(), 4);
        let flat = tree.flatten();
        assert!(flat.iter().all(|(d, _)| *d == 0));
    }

    #[test]
    fn activating_directory_toggles_expansion_and_loads_children() {
        let td = TempDir::new().unwrap();
        let root = mk_tree(&td);
        let mut tree = FileTree::new(root);
        // "src" は dirs 昇順なので index=1 (docs=0, src=1)
        let result = tree.activate_at(1);
        assert!(matches!(result, Some(ActivateResult::DirToggled)));
        assert_eq!(tree.visible_len(), 6); // docs, src(展開), src/lib.rs, src/main.rs, Cargo.toml, README.md
        let flat = tree.flatten();
        // src(depth=0) の直下に lib.rs(depth=1), main.rs(depth=1) が並ぶ
        let depth_one_names: Vec<&str> = flat
            .iter()
            .filter(|(d, _)| *d == 1)
            .map(|(_, n)| n.name.as_str())
            .collect();
        assert_eq!(depth_one_names, vec!["lib.rs", "main.rs"]);
    }

    #[test]
    fn activating_file_returns_path_without_changing_visible_count() {
        let td = TempDir::new().unwrap();
        let root = mk_tree(&td);
        let mut tree = FileTree::new(root);
        let before = tree.visible_len();
        // "Cargo.toml" は dirs(2) の後の files で index=2
        let result = tree.activate_at(2);
        match result {
            Some(ActivateResult::File(p)) => assert_eq!(p.file_name().unwrap(), "Cargo.toml"),
            other => panic!("expected File path, got {other:?}"),
        }
        assert_eq!(tree.visible_len(), before);
    }

    #[test]
    fn refresh_preserves_expansion() {
        let td = TempDir::new().unwrap();
        let root = mk_tree(&td);
        let mut tree = FileTree::new(root);
        tree.activate_at(1); // expand src
        assert_eq!(tree.visible_len(), 6);
        tree.refresh();
        assert_eq!(tree.visible_len(), 6);
    }

    #[test]
    fn icon_for_directory_is_folder_emoji() {
        assert_eq!(icon_for_kind(EntryKind::Directory), "📁");
    }

    #[test]
    fn classify_recognizes_common_extensions() {
        let root = Path::new("workspace");
        assert_eq!(classify_path(&root.join("a.rs"), false), EntryKind::Rust);
        assert_eq!(classify_path(&root.join("a.py"), false), EntryKind::Python);
        assert_eq!(
            classify_path(&root.join(".gitignore"), false),
            EntryKind::Git
        );
        assert_eq!(
            classify_path(&root.join("Cargo.lock"), false),
            EntryKind::Lock
        );
        assert_eq!(classify_path(&root.join("anything"), true), EntryKind::Directory);
    }
}

// Version History
// ver0.1 - 2026-04-25 - Added file tree icon and kind metadata with classification tests.
// ver0.2 - 2026-04-25 - Replaced flat walk with lazy expand/collapse FileTree (folder-first sort).
