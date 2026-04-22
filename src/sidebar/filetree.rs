use std::path::{Path, PathBuf};

use anyhow::Result;
use ignore::WalkBuilder;

#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub depth: usize,
    pub is_dir: bool,
}

impl Entry {
    pub fn display(&self, root: &Path) -> String {
        let rel = self.path.strip_prefix(root).unwrap_or(&self.path);
        let name = rel
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| rel.to_string_lossy().to_string());
        let indent = "  ".repeat(self.depth.saturating_sub(1));
        let marker = if self.is_dir { "/" } else { "" };
        format!("{indent}{name}{marker}")
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
