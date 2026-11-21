//! 手で付けたタブ名の保存 (`%APPDATA%\ccnest\tab-names.json`)。
//!
//! タブをダブルクリック (または Alt+F) して付けた名前を、そのタブのペインの
//! セッション id ごとに覚える。窓を閉じても、再起動後の `--resume <id>` や
//! `ccnest --add` で同じセッションを開けば同じ名前で出る。
//!
//! - ファイルは `セッション id → { name, at }` の JSON。`at` は付けた時刻
//!   (UNIX 秒) で、[`MAX_ENTRIES`] を超えたときに古い順に落とすためだけに使う
//! - 複数の窓が同じファイルを使うので、書くときは必ず読み直してから 1 件だけ
//!   足す／消す (ほかの窓が付けた名前を消さない)
//! - 書き込みは一時ファイル → rename (`claude::jp_title` の保存と同じ型)
//! - `CCNEST_TAB_NAMES=<path>` で置き場所を変えられる (E2E が本物のファイルに
//!   触らないため)
//!
//! Claude Code 本体の `/rename` (会話ログの `custom-title` 行) には書かない。
//! 動いている claude の会話ログへ外から行を足すと、返答中の書き込みとぶつかる。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 覚えておく名前の数。超えたら付けた時刻の古い順に落とす。
pub const MAX_ENTRIES: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    /// 付けた時刻 (UNIX 秒)。
    #[serde(default)]
    pub at: i64,
}

/// セッション id → 名前。
pub type Names = HashMap<String, Entry>;

/// 保存先。`CCNEST_TAB_NAMES` があればそのパス、無ければ
/// `%APPDATA%\ccnest\tab-names.json`。
pub fn store_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CCNEST_TAB_NAMES").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    Some(dirs::data_dir()?.join("ccnest").join("tab-names.json"))
}

/// ファイルが無い・壊れているときは空 (名前が出ないだけで、動作は止めない)。
pub fn load(path: &Path) -> Names {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// 一時ファイルに書いてから rename (ほかの窓に書きかけを読ませない)。
fn save(path: &Path, names: &Names) {
    let Ok(text) = serde_json::to_string(names) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// `ids` の全部に `name` を付ける。[`MAX_ENTRIES`] を超えた分は古い順に落とす。
pub fn apply_set(names: &mut Names, ids: &[String], name: &str, now: i64) {
    for id in ids {
        names.insert(
            id.clone(),
            Entry {
                name: name.to_string(),
                at: now,
            },
        );
    }
    prune(names, MAX_ENTRIES);
}

/// `ids` の名前を外す。
pub fn apply_remove(names: &mut Names, ids: &[String]) {
    for id in ids {
        names.remove(id);
    }
}

/// `max` 件を超えた分を、付けた時刻の古い順に落とす (同じ時刻は id の順)。
fn prune(names: &mut Names, max: usize) {
    if names.len() <= max {
        return;
    }
    let mut order: Vec<(i64, String)> = names.iter().map(|(k, e)| (e.at, k.clone())).collect();
    order.sort();
    let drop = names.len() - max;
    for (_, id) in order.into_iter().take(drop) {
        names.remove(&id);
    }
}

/// 読み直してから `ids` に `name` を付けて保存する。
pub fn set(path: &Path, ids: &[String], name: &str, now: i64) {
    let mut names = load(path);
    apply_set(&mut names, ids, name, now);
    save(path, &names);
}

/// 読み直してから `ids` の名前を外して保存する。もともと 1 件も無ければ書かない。
pub fn remove(path: &Path, ids: &[String]) {
    let mut names = load(path);
    let before = names.len();
    apply_remove(&mut names, ids);
    if names.len() != before {
        save(path, &names);
    }
}

/// セッション `id` に付けた名前。
pub fn lookup(path: &Path, id: &str) -> Option<String> {
    load(path).remove(id).map(|e| e.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn set_then_lookup_roundtrips_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("tab-names.json");
        assert_eq!(lookup(&path, "a"), None, "ファイルが無ければ None");
        set(&path, &ids(&["a", "b"]), "決済まわり", 100);
        assert_eq!(lookup(&path, "a").as_deref(), Some("決済まわり"));
        assert_eq!(lookup(&path, "b").as_deref(), Some("決済まわり"));
        assert_eq!(lookup(&path, "c"), None);
    }

    #[test]
    fn set_overwrites_the_name_of_the_same_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tab-names.json");
        set(&path, &ids(&["a"]), "old", 1);
        set(&path, &ids(&["a"]), "new", 2);
        assert_eq!(lookup(&path, "a").as_deref(), Some("new"));
        assert_eq!(load(&path).len(), 1);
    }

    /// ほかの窓が先に書いた分を消さない (書く前に読み直す)。
    #[test]
    fn set_keeps_entries_written_by_another_window() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tab-names.json");
        set(&path, &ids(&["other-window"]), "別の窓", 1);
        set(&path, &ids(&["mine"]), "この窓", 2);
        assert_eq!(lookup(&path, "other-window").as_deref(), Some("別の窓"));
        assert_eq!(lookup(&path, "mine").as_deref(), Some("この窓"));
    }

    #[test]
    fn remove_drops_only_the_given_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tab-names.json");
        set(&path, &ids(&["a", "b"]), "x", 1);
        remove(&path, &ids(&["a", "nope"]));
        assert_eq!(lookup(&path, "a"), None);
        assert_eq!(lookup(&path, "b").as_deref(), Some("x"));
    }

    /// 消す相手が無いときはファイルを作らない。
    #[test]
    fn remove_of_unknown_session_does_not_create_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tab-names.json");
        remove(&path, &ids(&["a"]));
        assert!(!path.exists());
    }

    #[test]
    fn broken_file_reads_as_empty_and_is_replaced_on_set() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tab-names.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(load(&path).is_empty());
        set(&path, &ids(&["a"]), "x", 1);
        assert_eq!(lookup(&path, "a").as_deref(), Some("x"));
    }

    #[test]
    fn prune_drops_the_oldest_entries_beyond_the_limit() {
        let mut names = Names::new();
        for i in 0..5i64 {
            apply_set(&mut names, &ids(&[&format!("s{i}")]), "n", i);
        }
        prune(&mut names, 3);
        let mut left: Vec<&str> = names.keys().map(String::as_str).collect();
        left.sort();
        assert_eq!(left, ["s2", "s3", "s4"]);
    }

    #[test]
    fn apply_set_never_grows_past_max_entries() {
        let mut names = Names::new();
        for i in 0..(MAX_ENTRIES as i64 + 20) {
            apply_set(&mut names, &ids(&[&format!("s{i}")]), "n", i);
        }
        assert_eq!(names.len(), MAX_ENTRIES);
        assert!(!names.contains_key("s0"), "いちばん古い分は落ちる");
        assert!(names.contains_key(&format!("s{}", MAX_ENTRIES as i64 + 19)));
    }
}

// Version History
// ver0.1 - 2026-11-21 - New: names given to a tab by hand (double-click a tab /
//                       Alt+F) are stored per session id in
//                       %APPDATA%\ccnest\tab-names.json so the same session
//                       shows the same name after the window is closed and the
//                       session is reopened with --resume / --add. Writes
//                       re-read the file first (several windows share it), go
//                       through a temp file, and keep at most 500 entries.
//                       CCNEST_TAB_NAMES overrides the path (E2E).
