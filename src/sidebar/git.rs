use std::path::Path;

use anyhow::Result;
use git2::{Repository, StatusOptions};

#[derive(Debug, Clone, Default)]
pub struct GitInfo {
    pub branch: String,
    pub modified: usize,
    pub staged: usize,
    pub untracked: usize,
}

impl GitInfo {
    pub fn summary_line(&self) -> String {
        format!(
            "⎇ {}  M{} S{} ?{}",
            self.branch, self.modified, self.staged, self.untracked
        )
    }
}

pub fn load(path: &Path) -> Result<GitInfo> {
    let repo = Repository::discover(path)?;
    let branch = match repo.head() {
        Ok(r) => r.shorthand().unwrap_or("HEAD").to_string(),
        Err(_) => "(detached)".to_string(),
    };
    let mut opts = StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true);
    let statuses = repo.statuses(Some(&mut opts))?;
    let mut info = GitInfo {
        branch,
        ..Default::default()
    };
    for s in statuses.iter() {
        let st = s.status();
        if st.is_wt_new() {
            info.untracked += 1;
        } else if st.is_wt_modified() || st.is_wt_deleted() || st.is_wt_renamed() {
            info.modified += 1;
        } else if st.is_index_new()
            || st.is_index_modified()
            || st.is_index_deleted()
            || st.is_index_renamed()
        {
            info.staged += 1;
        }
    }
    Ok(info)
}
