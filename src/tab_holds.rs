//! 保留にしたタブの保存 (`%APPDATA%\ccnest\tab-holds.json`)。
//!
//! Alt+C で「保留」にしたタブを、そのタブのペインのセッション id ごとに覚える。
//! 窓を閉じても、再起動後の `--resume <id>` や `ccnest --add` で同じセッションを
//! 開けば保留のまま (灰色のマーカー + 番号だけ) で出る。
//!
//! - ファイルは `セッション id → 保留にした時刻 (UNIX 秒)` の JSON。時刻は
//!   [`MAX_ENTRIES`] を超えたときに古い順に落とすためだけに使う
//! - 複数の窓が同じファイルを使うので、書くときは必ず読み直してから足す／消す
//!   (ほかの窓の保留を消さない)。[`crate::tab_names`] と同じ型
//! - 書き込みは一時ファイル → rename
//! - `CCNEST_TAB_HOLDS=<path>` で置き場所を変えられる (E2E が本物のファイルに
//!   触らないため)

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 覚えておく保留の数。超えたら保留にした時刻の古い順に落とす。
pub const MAX_ENTRIES: usize = 500;

/// セッション id → 保留にした時刻 (UNIX 秒)。
pub type Holds = HashMap<String, i64>;

/// 保存先。`CCNEST_TAB_HOLDS` があればそのパス、無ければ
/// `%APPDATA%\ccnest\tab-holds.json`。
pub fn store_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CCNEST_TAB_HOLDS").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    Some(dirs::data_dir()?.join("ccnest").join("tab-holds.json"))
}

/// ファイルが無い・壊れているときは空 (保留が外れて見えるだけで、動作は止めない)。
pub fn load(path: &Path) -> Holds {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// 一時ファイルに書いてから rename (ほかの窓に書きかけを読ませない)。
fn save(path: &Path, holds: &Holds) {
    let Ok(text) = serde_json::to_string(holds) else {
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

/// `ids` の全部を保留にする。[`MAX_ENTRIES`] を超えた分は古い順に落とす。
pub fn apply_set(holds: &mut Holds, ids: &[String], now: i64) {
    for id in ids {
        holds.insert(id.clone(), now);
    }
    prune(holds, MAX_ENTRIES);
}

/// `ids` の保留を外す。
pub fn apply_remove(holds: &mut Holds, ids: &[String]) {
    for id in ids {
        holds.remove(id);
    }
}

/// `max` 件を超えた分を、保留にした時刻の古い順に落とす (同じ時刻は id の順)。
fn prune(holds: &mut Holds, max: usize) {
    if holds.len() <= max {
        return;
    }
    let mut order: Vec<(i64, String)> = holds.iter().map(|(k, at)| (*at, k.clone())).collect();
    order.sort();
    let drop = holds.len() - max;
    for (_, id) in order.into_iter().take(drop) {
        holds.remove(&id);
    }
}

/// 読み直してから `ids` を保留にして保存する。
pub fn set(path: &Path, ids: &[String], now: i64) {
    let mut holds = load(path);
    apply_set(&mut holds, ids, now);
    save(path, &holds);
}

/// 読み直してから `ids` の保留を外して保存する。もともと 1 件も無ければ書かない。
pub fn remove(path: &Path, ids: &[String]) {
    let mut holds = load(path);
    let before = holds.len();
    apply_remove(&mut holds, ids);
    if holds.len() != before {
        save(path, &holds);
    }
}

/// セッション `id` が保留か。
pub fn is_held(path: &Path, id: &str) -> bool {
    load(path).contains_key(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn set_then_is_held_roundtrips_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("tab-holds.json");
        assert!(!is_held(&path, "a"), "ファイルが無ければ保留ではない");
        set(&path, &ids(&["a", "b"]), 100);
        assert!(is_held(&path, "a"));
        assert!(is_held(&path, "b"));
        assert!(!is_held(&path, "c"));
    }

    /// ほかの窓が先に書いた分を消さない (書く前に読み直す)。
    #[test]
    fn set_keeps_entries_written_by_another_window() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tab-holds.json");
        set(&path, &ids(&["other-window"]), 1);
        set(&path, &ids(&["mine"]), 2);
        assert!(is_held(&path, "other-window"));
        assert!(is_held(&path, "mine"));
    }

    #[test]
    fn remove_drops_only_the_given_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tab-holds.json");
        set(&path, &ids(&["a", "b"]), 1);
        remove(&path, &ids(&["a", "nope"]));
        assert!(!is_held(&path, "a"));
        assert!(is_held(&path, "b"));
    }

    /// 消す相手が無いときはファイルを作らない。
    #[test]
    fn remove_of_unknown_session_does_not_create_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tab-holds.json");
        remove(&path, &ids(&["a"]));
        assert!(!path.exists());
    }

    #[test]
    fn broken_file_reads_as_empty_and_is_replaced_on_set() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tab-holds.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(load(&path).is_empty());
        set(&path, &ids(&["a"]), 1);
        assert!(is_held(&path, "a"));
    }

    #[test]
    fn apply_set_never_grows_past_max_entries() {
        let mut holds = Holds::new();
        for i in 0..(MAX_ENTRIES as i64 + 20) {
            apply_set(&mut holds, &ids(&[&format!("s{i}")]), i);
        }
        assert_eq!(holds.len(), MAX_ENTRIES);
        assert!(!holds.contains_key("s0"), "いちばん古い分は落ちる");
        assert!(holds.contains_key(&format!("s{}", MAX_ENTRIES as i64 + 19)));
    }
}

// Version History
// ver0.1 - 2026-11-21 - New: tabs put on hold with Alt+C are stored per session
//                       id in %APPDATA%\ccnest\tab-holds.json so the same
//                       session is still on hold after the window is closed and
//                       the session is reopened with --resume / --add. Same
//                       shape as tab_names (re-read before write, temp file,
//                       at most 500 entries). CCNEST_TAB_HOLDS overrides the
//                       path (E2E).
