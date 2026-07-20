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

/// ブランチ名だけを取る軽量版。`load()` と違い status の全ツリー走査を
/// しないので、フォーカスごと・tick ごとに呼んでも安い。
///
/// リポジトリ外なら `None`。detached HEAD は `load()` と同じ表記に揃える。
pub fn branch_of(path: &Path) -> Option<String> {
    let repo = Repository::discover(path).ok()?;
    Some(head_branch(&repo))
}

fn head_branch(repo: &Repository) -> String {
    // 注意: detached HEAD では head() は **Ok** を返し、shorthand が短縮 SHA に
    // なる。head() が Err になるのは主に「まだコミットが 1 つも無いブランチ
    // (unborn)」で、そこを一律 "(detached)" と呼ぶのは誤り。unborn のときは
    // HEAD のシンボリック参照からブランチ名を復元する。
    if let Ok(r) = repo.head() {
        return r.shorthand().unwrap_or("HEAD").to_string();
    }
    repo.find_reference("HEAD")
        .ok()
        .and_then(|h| h.symbolic_target().map(|t| t.to_string()))
        .map(|t| t.strip_prefix("refs/heads/").unwrap_or(&t).to_string())
        .unwrap_or_else(|| "(detached)".to_string())
}

pub fn load(path: &Path) -> Result<GitInfo> {
    let repo = Repository::discover(path)?;
    let branch = head_branch(&repo);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_of_returns_none_outside_repo() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(branch_of(dir.path()), None);
    }

    #[test]
    fn branch_of_reports_unborn_branch_by_name() {
        // git init 直後 (コミット 0 件) は head() が Err になるが、
        // detached ではなく普通のブランチ上にいる。
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.set_head("refs/heads/demo-branch").unwrap();
        assert_eq!(branch_of(dir.path()).as_deref(), Some("demo-branch"));
    }
}
