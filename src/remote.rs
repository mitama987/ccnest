//! 動いている ccnest の窓へ「このセッションをタブで開いて」を届ける口
//! (`ccnest --add [--to <pid>] --resume <uuid>[=<タブ名>] ...`)。
//!
//! パイプやソケットは使わず、依頼ファイルの受け渡しにしている。窓側は
//! 既に「`pending_resumes` を 2 秒 tick で 1 タブずつ開く」流れを持っている
//! ので、そこへ積むだけで済む (新しいスレッドも依存も要らない)。
//!
//! - 窓側 ([`Inbox`]): 起動時に `<data_dir>/ccnest/requests/<自分の pid>/` を作り、
//!   2 秒 tick ごとに [`take_requests`] で `*.json` を拾う。検査 ([`plan_requests`])
//!   の結果を `<同名>.done` に書いて依頼を消す。終了時にフォルダを消す。
//! - 送る側 ([`send`]): 依頼を `.json.tmp` に書いてから `.json` へ rename し
//!   (書きかけを読ませない)、`.done` を待って結果を返す。
//! - 宛先 ([`resolve_target`]): `--to <pid>` → 環境変数 [`PID_ENV`]。ccnest は
//!   ペインの子へ `CCNEST_PID=<自分の pid>` を渡すので、タブの中から打てば
//!   「この窓」に届く。

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::cli::ResumeSpec;

/// ccnest がペインの子プロセスへ渡す「この窓の pid」。
pub const PID_ENV: &str = "CCNEST_PID";

/// 送る側が受け取らなかった `.done` を窓側が片付けるまでの時間。
const STALE_REPLY: Duration = Duration::from_secs(60);

/// 依頼 1 件 (= 依頼ファイル 1 つ)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub resumes: Vec<ResumeSpec>,
}

/// 開かなかった理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// その窓ですでに開いている (開き待ちを含む)。
    AlreadyOpen,
    /// その窓のフォルダに会話ログが無い (開いてもすぐ終わる)。
    NoSessionLog,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skipped {
    pub session_id: Uuid,
    pub reason: SkipReason,
}

/// 窓側の返事 (`.done` の中身)。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Reply {
    #[serde(default)]
    pub accepted: Vec<Uuid>,
    #[serde(default)]
    pub skipped: Vec<Skipped>,
    /// 依頼そのものが読めなかった (壊れた JSON など)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Reply {
    /// 頼んだセッションがすべて「その窓のタブにある (開いた／開いていた)」か。
    /// 送る側の終了コードに使う。
    pub fn all_present(&self) -> bool {
        self.error.is_none()
            && self
                .skipped
                .iter()
                .all(|s| s.reason == SkipReason::AlreadyOpen)
    }
}

/// 窓側が拾った依頼 1 件。
#[derive(Debug)]
pub struct Incoming {
    /// 返事を書く先 (`<依頼と同名>.done`)。
    pub reply_path: PathBuf,
    /// 読めなかった依頼は理由つきの Err (送る側へそのまま返す)。
    pub request: Result<Request, String>,
}

/// 依頼フォルダの根 (`<data_dir>/ccnest/requests`)。
pub fn requests_root() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("ccnest").join("requests"))
}

/// pid ごとの依頼フォルダ。
pub fn inbox_dir(root: &Path, pid: u32) -> PathBuf {
    root.join(pid.to_string())
}

/// 純粋: 宛先の pid。`--to` が最優先、無ければ環境変数 [`PID_ENV`] の値。
pub fn resolve_target(to: Option<u32>, env: Option<&str>) -> Result<u32> {
    if let Some(pid) = to {
        return Ok(pid);
    }
    let raw = env.map(str::trim).filter(|v| !v.is_empty()).ok_or_else(|| {
        anyhow!(
            "宛先の ccnest が分かりません。ccnest のタブの中から実行するか、--to <pid> を付けてください"
        )
    })?;
    raw.parse::<u32>()
        .map_err(|e| anyhow!("{PID_ENV} が pid ではありません ({raw:?}): {e}"))
}

/// 純粋: 依頼されたセッションを「開く」と「開かない (理由つき)」に分ける。
/// `open` はその窓で開いている／開き待ちのセッション、`has_log` は会話ログの有無。
/// 同じ依頼の中の重複は 2 つ目以降を `AlreadyOpen` にする。
pub fn plan_requests(
    specs: Vec<ResumeSpec>,
    open: &HashSet<Uuid>,
    has_log: impl Fn(&Uuid) -> bool,
) -> (Vec<ResumeSpec>, Vec<Skipped>) {
    let mut seen = open.clone();
    let mut accepted = Vec::new();
    let mut skipped = Vec::new();
    for spec in specs {
        let reason = if seen.contains(&spec.session_id) {
            Some(SkipReason::AlreadyOpen)
        } else if !has_log(&spec.session_id) {
            Some(SkipReason::NoSessionLog)
        } else {
            None
        };
        match reason {
            Some(reason) => skipped.push(Skipped {
                session_id: spec.session_id,
                reason,
            }),
            None => {
                seen.insert(spec.session_id);
                accepted.push(spec);
            }
        }
    }
    (accepted, skipped)
}

/// 依頼フォルダの `*.json` を名前順 (= 届いた順) に読み、依頼ファイルを消して返す。
/// 書きかけ (`.json.tmp`) は読まない。古い `.done` もここで片付ける。
pub fn take_requests(dir: &Path) -> Vec<Incoming> {
    take_requests_with(dir, STALE_REPLY)
}

fn take_requests_with(dir: &Path, stale_reply: Duration) -> Vec<Incoming> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut requests: Vec<PathBuf> = Vec::new();
    for path in entries.flatten().map(|e| e.path()) {
        match path.extension().and_then(|e| e.to_str()) {
            Some("json") => requests.push(path),
            Some("done") => {
                let age = fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok());
                if age.is_some_and(|a| a >= stale_reply) {
                    let _ = fs::remove_file(&path);
                }
            }
            _ => {}
        }
    }
    requests.sort();
    requests
        .into_iter()
        .map(|path| {
            let request = fs::read_to_string(&path)
                .map_err(|e| format!("依頼を読めません: {e}"))
                .and_then(|text| {
                    serde_json::from_str::<Request>(&text)
                        .map_err(|e| format!("依頼の形式が違います: {e}"))
                });
            // 読めても読めなくても消す (壊れた依頼を毎 tick 読み直さない)。
            let _ = fs::remove_file(&path);
            Incoming {
                reply_path: path.with_extension("done"),
                request,
            }
        })
        .collect()
}

/// 返事を書く (`.tmp` → rename。送る側に書きかけを読ませない)。
pub fn write_reply(path: &Path, reply: &Reply) -> Result<()> {
    let tmp = path.with_extension("done.tmp");
    fs::write(&tmp, serde_json::to_vec(reply)?)
        .with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))
}

/// 送る側: 依頼を置いて返事を待つ。`dir` が無い (その pid の ccnest が動いて
/// いない／この機能より前の版) なら即エラー。`timeout` までに返事が無ければ
/// 依頼を回収してエラー。
pub fn send(dir: &Path, specs: &[ResumeSpec], timeout: Duration) -> Result<Reply> {
    if !dir.is_dir() {
        return Err(anyhow!(
            "宛先の ccnest が見つかりません (動いていないか、--add に対応していない古い版です): {}",
            dir.display()
        ));
    }
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    // 名前順 = 届いた順になるよう、先頭をゼロ埋めの時刻にする。
    let stem = format!(
        "{millis:013}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    );
    let request = dir.join(format!("{stem}.json"));
    let tmp = dir.join(format!("{stem}.json.tmp"));
    let done = dir.join(format!("{stem}.done"));
    let body = serde_json::to_vec(&Request {
        resumes: specs.to_vec(),
    })?;
    fs::write(&tmp, body).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, &request).with_context(|| format!("rename to {}", request.display()))?;

    let started = Instant::now();
    loop {
        if let Ok(text) = fs::read_to_string(&done) {
            let _ = fs::remove_file(&done);
            return serde_json::from_str(&text).context("ccnest からの返事を読めません");
        }
        if started.elapsed() >= timeout {
            // 置きっぱなしにしない (後から開かれて驚かせない)。
            let _ = fs::remove_file(&request);
            let _ = fs::remove_file(&done);
            return Err(anyhow!(
                "ccnest から {} 秒以内に返事がありませんでした (依頼は取り消しました)",
                timeout.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 送る側の表示 (1 行目が要約、以降 1 セッション 1 行)。
pub fn format_reply(pid: u32, specs: &[ResumeSpec], reply: &Reply) -> String {
    let title_of = |id: &Uuid| {
        specs
            .iter()
            .find(|s| s.session_id == *id)
            .and_then(|s| s.title.clone())
    };
    let mut out = match &reply.error {
        Some(e) => format!("ccnest {pid}: 依頼を受け付けられませんでした: {e}"),
        None => format!(
            "ccnest {pid} にタブを追加します: {} 件 (2 秒ごとに 1 つずつ開きます)",
            reply.accepted.len()
        ),
    };
    for id in &reply.accepted {
        match title_of(id) {
            Some(t) => out.push_str(&format!("\n  開く      {id}  {t}")),
            None => out.push_str(&format!("\n  開く      {id}")),
        }
    }
    for s in &reply.skipped {
        let why = match s.reason {
            SkipReason::AlreadyOpen => "すでに開いています",
            SkipReason::NoSessionLog => "この窓のフォルダに会話ログがありません",
        };
        out.push_str(&format!("\n  スキップ  {}  ({why})", s.session_id));
    }
    out
}

/// その pid のプロセスが動いているか。Windows 以外は調べず true
/// (片付けをしないだけで、動作は変わらない)。
pub fn pid_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
        };
        // SAFETY: OpenProcess は失敗時 0 を返す。得たハンドルは必ず CloseHandle する。
        // 待ち時間 0 の WaitForSingleObject は「終了済みか」を見るだけで待たない。
        unsafe {
            let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
            if handle == 0 {
                // いない pid は開けない。権限で開けないだけの他人のプロセスも
                // 「自分の ccnest ではない」ので false でよい。
                return false;
            }
            let alive = WaitForSingleObject(handle, 0) == WAIT_TIMEOUT;
            CloseHandle(handle);
            alive
        }
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        true
    }
}

/// 窓側の依頼フォルダ。Drop で消す。
#[derive(Debug)]
pub struct Inbox {
    dir: PathBuf,
}

impl Inbox {
    /// `root/<pid>/` を空で作る。ついでに、もう動いていない pid のフォルダを消す
    /// (窓を × で閉じると Drop が走らず残るため)。
    pub fn open(root: &Path, pid: u32, alive: impl Fn(u32) -> bool) -> Result<Self> {
        fs::create_dir_all(root).with_context(|| format!("create {}", root.display()))?;
        if let Ok(entries) = fs::read_dir(root) {
            for entry in entries.flatten() {
                let other = entry.file_name().to_string_lossy().parse::<u32>().ok();
                if other.is_some_and(|p| p != pid && !alive(p)) {
                    let _ = fs::remove_dir_all(entry.path());
                }
            }
        }
        let dir = inbox_dir(root, pid);
        // pid の使い回しで前の持ち主の依頼が残っていたら捨てる。
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        Ok(Self { dir })
    }

    /// この ccnest 自身の依頼フォルダ。作れなければ None (機能が無効になるだけ)。
    pub fn open_own() -> Option<Self> {
        let root = requests_root()?;
        Self::open(&root, std::process::id(), pid_alive).ok()
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl Drop for Inbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const A: &str = "7f460491-997d-48bd-9118-497e59f4bc95";
    const B: &str = "d86c5a88-97f6-4d96-a6c1-5433ec41d932";
    const C: &str = "0b0c3f0e-6a55-4b61-9d0a-2f1f3b8f7a11";

    fn id(s: &str) -> Uuid {
        Uuid::parse_str(s).unwrap()
    }

    fn spec(s: &str, title: Option<&str>) -> ResumeSpec {
        ResumeSpec {
            session_id: id(s),
            title: title.map(str::to_string),
        }
    }

    /// 宛先は --to が最優先。無ければ CCNEST_PID。どちらも無い／数字でないはエラー。
    #[test]
    fn resolve_target_prefers_to_then_env() {
        assert_eq!(resolve_target(Some(10), Some("20")).unwrap(), 10);
        assert_eq!(resolve_target(None, Some(" 20 ")).unwrap(), 20);
        assert!(resolve_target(None, None).is_err());
        assert!(resolve_target(None, Some("")).is_err());
        assert!(resolve_target(None, Some("abc")).is_err());
    }

    /// 開く／すでに開いている／会話ログが無い、の 3 分岐。
    #[test]
    fn plan_requests_splits_three_ways() {
        let open: HashSet<Uuid> = [id(B)].into_iter().collect();
        let (accepted, skipped) = plan_requests(
            vec![spec(A, Some("a")), spec(B, None), spec(C, None)],
            &open,
            |u| *u != id(C),
        );
        assert_eq!(accepted, vec![spec(A, Some("a"))]);
        assert_eq!(
            skipped,
            vec![
                Skipped {
                    session_id: id(B),
                    reason: SkipReason::AlreadyOpen
                },
                Skipped {
                    session_id: id(C),
                    reason: SkipReason::NoSessionLog
                },
            ]
        );
    }

    /// 同じ依頼の中で同じセッションを 2 回頼まれたら、開くのは 1 回だけ。
    #[test]
    fn plan_requests_dedupes_within_one_request() {
        let (accepted, skipped) =
            plan_requests(vec![spec(A, None), spec(A, None)], &HashSet::new(), |_| {
                true
            });
        assert_eq!(accepted, vec![spec(A, None)]);
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].reason, SkipReason::AlreadyOpen);
    }

    /// 開いていて、かつ会話ログも無いなら「すでに開いている」を先に返す。
    #[test]
    fn plan_requests_already_open_wins_over_missing_log() {
        let open: HashSet<Uuid> = [id(A)].into_iter().collect();
        let (accepted, skipped) = plan_requests(vec![spec(A, None)], &open, |_| false);
        assert!(accepted.is_empty());
        assert_eq!(skipped[0].reason, SkipReason::AlreadyOpen);
    }

    #[test]
    fn reply_all_present() {
        let mut r = Reply {
            accepted: vec![id(A)],
            ..Reply::default()
        };
        assert!(r.all_present());
        r.skipped.push(Skipped {
            session_id: id(B),
            reason: SkipReason::AlreadyOpen,
        });
        assert!(r.all_present(), "すでに開いているのは成功扱い");
        r.skipped.push(Skipped {
            session_id: id(C),
            reason: SkipReason::NoSessionLog,
        });
        assert!(!r.all_present());
        let broken = Reply {
            error: Some("x".to_string()),
            ..Reply::default()
        };
        assert!(!broken.all_present());
    }

    /// 依頼は名前順に読まれて消える。書きかけ (.tmp) は読まず、残す。
    #[test]
    fn take_requests_reads_json_in_order_and_skips_tmp() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        let req = |s: &str| {
            serde_json::to_string(&Request {
                resumes: vec![spec(s, None)],
            })
            .unwrap()
        };
        fs::write(d.join("0002-1-0.json"), req(B)).unwrap();
        fs::write(d.join("0001-1-0.json"), req(A)).unwrap();
        fs::write(d.join("0003-1-0.json.tmp"), req(C)).unwrap();

        let got = take_requests(d);
        assert_eq!(got.len(), 2);
        assert_eq!(
            got[0].request.as_ref().unwrap().resumes[0].session_id,
            id(A)
        );
        assert_eq!(
            got[1].request.as_ref().unwrap().resumes[0].session_id,
            id(B)
        );
        assert_eq!(got[0].reply_path, d.join("0001-1-0.done"));
        assert!(!d.join("0001-1-0.json").exists(), "読んだ依頼は消す");
        assert!(!d.join("0002-1-0.json").exists());
        assert!(d.join("0003-1-0.json.tmp").exists(), "書きかけは触らない");
        assert!(take_requests(d).is_empty(), "2 回目は空");
    }

    /// 壊れた依頼は Err で返して消す (毎 tick 読み直さない)。
    #[test]
    fn take_requests_reports_broken_json_and_removes_it() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        fs::write(d.join("0001-1-0.json"), "{not json").unwrap();
        let got = take_requests(d);
        assert_eq!(got.len(), 1);
        assert!(got[0].request.is_err());
        assert!(!d.join("0001-1-0.json").exists());
    }

    /// 無いフォルダは空 (窓側はフォルダを作れなくても落ちない)。
    #[test]
    fn take_requests_on_missing_dir_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(take_requests(&tmp.path().join("nope")).is_empty());
    }

    /// 受け取られなかった古い .done は片付ける。新しい .done は残す。
    #[test]
    fn take_requests_removes_stale_replies_only() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        fs::write(d.join("0001-1-0.done"), "{}").unwrap();
        assert!(take_requests_with(d, Duration::from_secs(3600)).is_empty());
        assert!(d.join("0001-1-0.done").exists());
        assert!(take_requests_with(d, Duration::ZERO).is_empty());
        assert!(!d.join("0001-1-0.done").exists());
    }

    /// 送る → 窓側が拾って返事 → 送る側が返事を受け取り、フォルダは空に戻る。
    #[test]
    fn send_round_trips_with_a_polling_server() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().to_path_buf();
        let server_dir = d.clone();
        let server = std::thread::spawn(move || {
            for _ in 0..200 {
                if let Some(inc) = take_requests(&server_dir).into_iter().next() {
                    let req = inc.request.unwrap();
                    let reply = Reply {
                        accepted: req.resumes.iter().map(|r| r.session_id).collect(),
                        ..Reply::default()
                    };
                    write_reply(&inc.reply_path, &reply).unwrap();
                    return req;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            panic!("server saw no request");
        });
        let specs = [spec(A, Some("日本語 タブ")), spec(B, None)];
        let reply = send(&d, &specs, Duration::from_secs(5)).unwrap();
        let seen = server.join().unwrap();
        assert_eq!(seen.resumes, specs, "タブ名ごと届く");
        assert_eq!(reply.accepted, vec![id(A), id(B)]);
        assert_eq!(fs::read_dir(&d).unwrap().count(), 0, "後始末して空に戻る");
    }

    /// 依頼フォルダが無い = その pid の ccnest が動いていない → 即エラー。
    #[test]
    fn send_fails_fast_without_inbox() {
        let tmp = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let err = send(
            &tmp.path().join("4242"),
            &[spec(A, None)],
            Duration::from_secs(5),
        );
        assert!(err.is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    /// 返事が来なければ依頼を回収してエラー (置きっぱなしにしない)。
    #[test]
    fn send_times_out_and_withdraws_the_request() {
        let tmp = tempfile::tempdir().unwrap();
        let err = send(tmp.path(), &[spec(A, None)], Duration::from_millis(200));
        assert!(err.is_err());
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    /// 起動時: 動いていない pid のフォルダを消し、自分のフォルダは空で作り直す。
    /// 終了時 (Drop): 自分のフォルダを消す。
    #[test]
    fn inbox_open_prunes_dead_pids_and_drop_removes_own() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("requests");
        fs::create_dir_all(root.join("111")).unwrap(); // 死んでいる
        fs::create_dir_all(root.join("222")).unwrap(); // 生きている
        fs::create_dir_all(root.join("not-a-pid")).unwrap(); // 触らない
        fs::create_dir_all(root.join("333")).unwrap(); // 自分 (pid 再利用の残り)
        fs::write(root.join("333").join("old.json"), "{}").unwrap();

        let inbox = Inbox::open(&root, 333, |pid| pid == 222).unwrap();
        assert_eq!(inbox.dir(), inbox_dir(&root, 333));
        assert!(!root.join("111").exists());
        assert!(root.join("222").exists());
        assert!(root.join("not-a-pid").exists());
        assert!(root.join("333").is_dir());
        assert_eq!(fs::read_dir(root.join("333")).unwrap().count(), 0);

        drop(inbox);
        assert!(!root.join("333").exists());
    }

    /// 根のフォルダが無くても作る (初回起動)。
    #[test]
    fn inbox_open_creates_missing_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("a").join("requests");
        let inbox = Inbox::open(&root, 7, |_| true).unwrap();
        assert!(inbox.dir().is_dir());
    }

    #[test]
    fn pid_alive_knows_this_process() {
        assert!(pid_alive(std::process::id()));
    }

    /// Windows では、いない pid を「動いていない」と判定できる。
    #[cfg(windows)]
    #[test]
    fn pid_alive_is_false_for_an_exited_process() {
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "exit", "0"])
            .spawn()
            .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(!pid_alive(pid));
    }

    /// 表示: 1 行目に件数、開いたタブはタブ名つき、開かなかったものは理由つき。
    #[test]
    fn format_reply_lists_each_session() {
        let specs = [spec(A, Some("v4.25")), spec(B, None), spec(C, None)];
        let reply = Reply {
            accepted: vec![id(A)],
            skipped: vec![
                Skipped {
                    session_id: id(B),
                    reason: SkipReason::AlreadyOpen,
                },
                Skipped {
                    session_id: id(C),
                    reason: SkipReason::NoSessionLog,
                },
            ],
            error: None,
        };
        let text = format_reply(15416, &specs, &reply);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].contains("15416") && lines[0].contains("1 件"));
        assert!(lines[1].contains(A) && lines[1].contains("v4.25"));
        assert!(lines[2].contains(B) && lines[2].contains("すでに開いています"));
        assert!(lines[3].contains(C) && lines[3].contains("会話ログがありません"));
    }

    /// 返事の JSON は欠けた項目を既定値で読める (版が違っても落ちない)。
    #[test]
    fn reply_json_tolerates_missing_fields() {
        let r: Reply = serde_json::from_str("{}").unwrap();
        assert_eq!(r, Reply::default());
        let s = serde_json::to_string(&Reply::default()).unwrap();
        assert!(!s.contains("error"));
    }
}

// Version History
// ver0.1 - 2026-11-11 - First version: file-drop requests so a running ccnest
//                       can be asked to open saved sessions as new tabs
//                       (`ccnest --add [--to <pid>] --resume <uuid>[=<title>]`).
//                       Inbox (per-pid folder, pruned on start, removed on
//                       drop), take_requests / plan_requests / write_reply on
//                       the window side, resolve_target / send / format_reply
//                       on the sender side.
