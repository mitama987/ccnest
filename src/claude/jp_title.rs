//! タブに出す「日本語の短い名前」を作る。
//!
//! Claude Code が付けるセッション名 (`ai-title` 行と端末タイトル) は本体に
//! 埋め込みの英語プロンプトで作られ、`fix-user-name-issues` のような英語の
//! kebab-case になる (設定では変えられない)。そこで ccnest 側で
//! `claude -p --model haiku` に 1 回だけ言い換えさせ、結果を保存して使い回す。
//!
//! - 生成は専用スレッド 1 本で 1 件ずつ (描画・入力を止めない。claude を並行
//!   起動して `~/.claude.json` を同時に書かないため)
//! - キーは `sessionId + ai-title + プロンプトの版`。話題が変わって ai-title が
//!   変わったとき (とプロンプトを変えたとき) だけ作り直す
//! - 結果は `<data_dir>/ccnest/jp-titles.json` に保存し、開き直しても再生成しない
//! - `CCNEST_JP_TITLE=off` で無効 (従来どおり英語の端末タイトル)

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// タブ名の上限文字数。ここでは語の途中で切らないよう長めに持ち、見た目の
/// 長さはタブ側 (16 セル・末尾「…」) に任せる。ここは暴走よけ。
const TITLE_MAX_CHARS: usize = 16;
/// モデルが説明文を返してきたとみなす長さ。これを超えたら採用しない。
const REJECT_OVER_CHARS: usize = 40;
/// 1 回の生成の待ち時間上限。超えたら打ち切って英語のまま。
const GENERATE_TIMEOUT: Duration = Duration::from_secs(30);

/// 例を足すと「X上限更新」「アプリ横長バナー化」のように要点だけ残す
/// 言い換えになる (例なしだと英語の直訳で長くなりがち・2026-10-24 試行)。
///
/// 2026-11-15 に作り直した (ユーザー要望「タブの文字を短く」「『続きから再開』の
/// ようなタイトルだと何のことか不明」):
/// - 目安を全角 8 文字 → 6 文字に。Haiku は字数を数えられないので、収まる例と
///   「長い語の言い換え」を書いて寄せる (字数の指定だけでは 15 文字の名前が返る)
/// - 対象 (製品名・固有名詞) を必ず入れ、動作だけの名前を禁じる。以前は
///   `resume-p1-affilia-features` + 最後の依頼「続きから再開して」→「続きから再開」
///   のように、内容の無い依頼文に引っぱられて対象が落ちていた
///
/// 文面を変えたら [`PROMPT_VERSION`] も上げる (保存済みの名前を作り直させる)。
/// 試すときは `scripts/dev/try_title_prompt.py` (この定数を読んで本体と同じ呼び方をする)。
const SYSTEM_PROMPT: &str =
    "会話の話題を表すタブ名を付けてください。日本語の短い名詞句 1 つだけを返す。\
長さは全角 6 文字まで (英数字は 2 文字で全角 1 文字と数える。「Affilia P1」「XTP6価格改定」\
「DNS変更」はどれも収まる)。超えそうなら、対象 (製品名・固有名詞) を残して動作や修飾を削り、\
カタカナの長い語は短い日本語に言い換える (リダイレクト → 転送、バックグラウンド → 裏、\
アカウント → 垢)。何についての話かを必ず入れ、話題に英語の名前があればそのまま残す。\
「再開」「続き」「修正」「確認」のような動作だけの名前にしない。「最後の依頼」は対象が\
分からないときの参考で、「続きから」「進めて」「まだだめ」のような内容の無い依頼は無視する。\
対象がどうしても分からないときは、話題をそのまま短く訳す。\
例: fix-login-redirect-loop → ログイン転送、port-shadowban-check-xtp6 → XTP6シャドバン、\
resume-p1-affilia-features → Affilia P1、namegear-dns-change → ネームギアDNS、\
ccnest-background-agent-blue-marker → ccnest青マーカー、\
revenue-disclosure-video-format → 収益公開動画。説明・引用符・句読点・絵文字は付けない。";

/// [`SYSTEM_PROMPT`] の版。保存キーの末尾に付け、古い文面で作った名前を使い回さない。
const PROMPT_VERSION: &str = "p2";

/// `ai-title` がすでに日本語 (非 ASCII を含む) ならそのまま使ってよい。
/// Claude Code は日本語の会話でまれに日本語の ai-title を付ける。
pub fn usable_as_is(ai_title: &str) -> bool {
    !ai_title.is_ascii()
}

/// モデルの出力をタブ名に整える。最初の空でない行だけを使い、引用符・
/// 見出し記号・末尾の句読点を落とす。説明文らしき長文は捨てる (None)。
pub fn clean_title(raw: &str) -> Option<String> {
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty())?;
    // 「タブ名」。 のように引用符と句読点が混ざって並ぶので、両端から同じ集合で落とす。
    const STRIP: &[char] = &[
        '"', '\'', '`', '「', '」', '『', '』', '*', '#', '-', ' ', '　', '。', '、', '.', ',',
        '！', '!',
    ];
    let t = line.trim_matches(STRIP);
    let n = t.chars().count();
    if n == 0 || n > REJECT_OVER_CHARS {
        return None;
    }
    Some(t.chars().take(TITLE_MAX_CHARS).collect())
}

/// Haiku に渡す入力。ai-title だけだと略語が多いので、最後の依頼文も添える。
pub fn build_input(ai_title: &str, last_user: Option<&str>) -> String {
    let mut s = format!("会話の話題: {ai_title}\n");
    if let Some(u) = last_user.map(str::trim).filter(|u| !u.is_empty()) {
        let u: String = u.chars().take(200).collect();
        s.push_str(&format!("最後の依頼: {u}\n"));
    }
    s
}

/// タブに出す名前の優先順位:
/// 1. 人が付けた名前 (`/rename`・`--name` の custom-title)
/// 2. ccnest が作った日本語の名前
/// 3. Claude Code の端末タイトル (英語。日本語ができるまでのつなぎ)
/// 4. 最後のユーザー発話
pub fn pick_task(
    custom: Option<&str>,
    jp: Option<String>,
    osc: Option<String>,
    last_user: Option<&str>,
) -> Option<String> {
    let custom = custom
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_string);
    custom
        .or(jp)
        .or(osc)
        .or_else(|| last_user.map(str::to_string))
}

/// 子の claude に渡さない環境変数。ccnest を Claude Code の中から起動した
/// ときに親セッションの印が漏れると、子が「子セッション」扱いになる
/// (再起動復元スクリプトで実際に踏んだ)。設定の置き場所 (CLAUDE_CONFIG_DIR) は残す。
pub fn should_strip_env(key: &str) -> bool {
    let k = key.to_ascii_uppercase();
    k != "CLAUDE_CONFIG_DIR" && (k.starts_with("CLAUDE") || k.starts_with("AI_AGENT"))
}

/// 保存キー: `sessionId + ai-title + プロンプトの版`。v0.1.40 までは版が無く
/// (`sessionId + ai-title`)、その形のキーはもう引かない (ファイルには残るが無害)。
fn cache_key(session_id: &str, ai_title: &str) -> String {
    format!("{session_id}\u{1f}{ai_title}\u{1f}{PROMPT_VERSION}")
}

fn cache_path() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("ccnest").join("jp-titles.json"))
}

fn load_cache(path: &Path) -> HashMap<String, String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// 一時ファイルに書いてから rename (書きかけを読まないため)。
fn save_cache(path: &Path, cache: &HashMap<String, String>) {
    let Ok(text) = serde_json::to_string(cache) else {
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

struct Job {
    key: String,
    input: String,
}

/// タブ名の生成係。イベントループの 2 秒 tick から [`JpTitler::lookup`] を呼ぶ。
pub struct JpTitler {
    cache: Arc<Mutex<HashMap<String, String>>>,
    /// 生成を頼んだキー (成功・失敗にかかわらず、この起動中は頼み直さない)。
    requested: HashSet<String>,
    /// None = 無効 (`CCNEST_JP_TITLE=off`・claude が見つからない・テスト)。
    tx: Option<Sender<Job>>,
}

impl JpTitler {
    /// 生成しない (キャッシュも読まない)。テストと無効時に使う。
    pub fn disabled() -> Self {
        Self {
            cache: Arc::default(),
            requested: HashSet::new(),
            tx: None,
        }
    }

    pub fn from_env() -> Self {
        let off = std::env::var("CCNEST_JP_TITLE")
            .map(|v| v.trim().eq_ignore_ascii_case("off"))
            .unwrap_or(false);
        let (Some(path), Some(bin), false) =
            (cache_path(), crate::claude::launcher::claude_on_path(), off)
        else {
            return Self::disabled();
        };
        let cache = Arc::new(Mutex::new(load_cache(&path)));
        let (tx, rx) = mpsc::channel::<Job>();
        let worker_cache = Arc::clone(&cache);
        let spawned = std::thread::Builder::new()
            .name("ccnest-jp-title".to_string())
            .spawn(move || {
                let workdir = path.parent().map(Path::to_path_buf);
                for job in rx {
                    let Some(title) = generate(&bin, workdir.as_deref(), &job.input) else {
                        continue;
                    };
                    let Ok(mut c) = worker_cache.lock() else {
                        return;
                    };
                    c.insert(job.key, title);
                    save_cache(&path, &c);
                }
            });
        Self {
            cache,
            requested: HashSet::new(),
            tx: spawned.ok().map(|_| tx),
        }
    }

    /// 日本語のタブ名を返す。まだ無ければ生成を頼んで None (次の tick 以降で出る)。
    /// `allow_request` が false のときは頼まない (claude の起動直後など)。
    pub fn lookup(
        &mut self,
        session_id: &str,
        ai_title: &str,
        last_user: Option<&str>,
        allow_request: bool,
    ) -> Option<String> {
        if usable_as_is(ai_title) {
            return clean_title(ai_title);
        }
        let key = cache_key(session_id, ai_title);
        if let Some(t) = self.cache.lock().ok()?.get(&key) {
            return Some(t.clone());
        }
        let tx = self.tx.as_ref()?;
        if allow_request && self.requested.insert(key.clone()) {
            let _ = tx.send(Job {
                key,
                input: build_input(ai_title, last_user),
            });
        }
        None
    }
}

/// `claude -p --model haiku` を 1 回呼ぶ。設定・CLAUDE.md・MCP・ツールを
/// 読ませないので入力は 800 トークン弱・2〜3 秒 (2026-10-24 実測)。
/// セッションは残さない。コンソールを持たせない (ccnest の raw mode を
/// 子が触らないように)。
fn generate(bin: &Path, workdir: Option<&Path>, input: &str) -> Option<String> {
    let mut cmd = Command::new(bin);
    cmd.args([
        "-p",
        "--model",
        "haiku",
        "--effort",
        "low",
        "--no-session-persistence",
        "--strict-mcp-config",
        "--tools",
        "",
        "--disable-slash-commands",
        "--setting-sources",
        "",
        "--system-prompt",
        SYSTEM_PROMPT,
    ])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    if let Some(dir) = workdir {
        let _ = std::fs::create_dir_all(dir);
        cmd.current_dir(dir);
    }
    for (k, _) in std::env::vars_os() {
        if should_strip_env(&k.to_string_lossy()) {
            cmd.env_remove(k);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = cmd.spawn().ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if started.elapsed() > GENERATE_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    clean_title(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn japanese_ai_title_is_used_as_is() {
        assert!(usable_as_is("タブ表示名の日本語化"));
        assert!(!usable_as_is("fix-user-name-issues"));
    }

    #[test]
    fn clean_title_strips_quotes_and_punctuation() {
        assert_eq!(
            clean_title("「タブ名変更」。\n").as_deref(),
            Some("タブ名変更")
        );
        assert_eq!(
            clean_title("\n\n  **収益公開動画の型**  ").as_deref(),
            Some("収益公開動画の型")
        );
    }

    #[test]
    fn clean_title_caps_length_and_rejects_prose() {
        // 少し長い程度ならそのまま (見た目はタブ側の「…」で収める)
        assert_eq!(
            clean_title("XTP6モノトーン版モック計画").as_deref(),
            Some("XTP6モノトーン版モック計画")
        );
        let t = clean_title(&"あ".repeat(REJECT_OVER_CHARS)).unwrap();
        assert_eq!(t.chars().count(), TITLE_MAX_CHARS);
        assert_eq!(clean_title(&"あ".repeat(REJECT_OVER_CHARS + 1)), None);
        assert_eq!(clean_title("  \n "), None);
    }

    #[test]
    fn build_input_adds_last_request_when_present() {
        assert_eq!(build_input("a-b", None), "会話の話題: a-b\n");
        assert_eq!(
            build_input("a-b", Some(" 依頼 ")),
            "会話の話題: a-b\n最後の依頼: 依頼\n"
        );
    }

    #[test]
    fn pick_task_priority() {
        let s = |v: &str| Some(v.to_string());
        // 人が付けた名前が最優先
        assert_eq!(
            pick_task(Some("手動"), s("日本語"), s("osc"), Some("発話")),
            s("手動")
        );
        // 次に日本語の名前 (英語の端末タイトルより前)
        assert_eq!(
            pick_task(None, s("日本語"), s("osc"), Some("発話")),
            s("日本語")
        );
        assert_eq!(pick_task(Some(" "), None, s("osc"), Some("発話")), s("osc"));
        assert_eq!(pick_task(None, None, None, Some("発話")), s("発話"));
        assert_eq!(pick_task(None, None, None, None), None);
    }

    #[test]
    fn strips_parent_claude_session_env_but_keeps_config_dir() {
        assert!(should_strip_env("CLAUDE_CODE_CHILD_SESSION"));
        assert!(should_strip_env("CLAUDECODE"));
        assert!(should_strip_env("AI_AGENT"));
        assert!(!should_strip_env("CLAUDE_CONFIG_DIR"));
        assert!(!should_strip_env("PATH"));
        assert!(!should_strip_env("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn disabled_titler_never_requests() {
        let mut t = JpTitler::disabled();
        assert_eq!(t.lookup("s", "fix-bug", None, true), None);
        // 日本語の ai-title は無効でもそのまま使う
        assert_eq!(
            t.lookup("s", "タブ表示名の日本語化", None, true).as_deref(),
            Some("タブ表示名の日本語化")
        );
    }

    #[test]
    fn cached_title_is_returned() {
        let mut t = JpTitler::disabled();
        t.cache
            .lock()
            .unwrap()
            .insert(cache_key("s", "fix-bug"), "バグ修正".to_string());
        assert_eq!(
            t.lookup("s", "fix-bug", None, false).as_deref(),
            Some("バグ修正")
        );
        assert_eq!(t.lookup("other", "fix-bug", None, false), None);
    }

    // プロンプトを変える前 (v0.1.40 まで) に保存した名前は使わない: キーの形が違う。
    // 「続きから再開」のように対象の無い名前が残り続けないよう、作り直させる
    #[test]
    fn titles_cached_with_an_older_prompt_are_not_reused() {
        let mut t = JpTitler::disabled();
        t.cache.lock().unwrap().insert(
            "s\u{1f}resume-p1-affilia-features".to_string(),
            "続きから再開".to_string(),
        );
        assert_eq!(
            t.lookup("s", "resume-p1-affilia-features", None, false),
            None
        );
        assert!(cache_key("s", "a").ends_with(PROMPT_VERSION));
        assert_ne!(cache_key("s", "a"), "s\u{1f}a");
    }

    #[test]
    fn request_is_sent_once_and_only_when_allowed() {
        let (tx, rx) = mpsc::channel();
        let mut t = JpTitler {
            tx: Some(tx),
            ..JpTitler::disabled()
        };
        assert_eq!(t.lookup("s", "fix-bug", Some("直して"), false), None);
        assert!(rx.try_recv().is_err());
        assert_eq!(t.lookup("s", "fix-bug", Some("直して"), true), None);
        assert_eq!(t.lookup("s", "fix-bug", Some("直して"), true), None);
        let job = rx.try_recv().unwrap();
        assert_eq!(job.input, "会話の話題: fix-bug\n最後の依頼: 直して\n");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn cache_round_trips_through_file() {
        let dir = std::env::temp_dir().join(format!("ccnest-jp-title-{}", std::process::id()));
        let path = dir.join("jp-titles.json");
        let mut m = HashMap::new();
        m.insert(cache_key("s", "fix-bug"), "バグ修正".to_string());
        save_cache(&path, &m);
        assert_eq!(load_cache(&path), m);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ---------------------------------------------------------------------------
// Version History
// ver0.2 - 2026-11-15 - タブ名を短く・何の話か分かる名前に (ユーザー要望)。
//                       プロンプトを作り直し (全角 6 文字目安・対象を必ず入れる・
//                       動作だけの名前と内容の無い依頼文を使わない)、保存キーに
//                       PROMPT_VERSION を足して古い名前を作り直させる。
// ver0.1 - 2026-10-24 - 新規。英語の ai-title を claude -p (haiku) で日本語の
//                       短いタブ名に言い換え、jp-titles.json に保存して使い回す。
//                       日本語の ai-title はそのまま。CCNEST_JP_TITLE=off で無効。
