use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;
use serde::Deserialize;

/// 既知のコンテキストウィンドウの段階 (小さい順)。
///
/// **セッションがどちらの窓で動いているかを知る手段はディスク上に存在しない。**
/// transcript の `message.model` は `[1m]` サフィックスを落とすし、
/// `~/.claude/sessions/<pid>.json` に model フィールドは無く、`.claude.json` の
/// `lastModelUsage` は「前回セッションの複数モデル合算」で当てにならない。
///
/// そこで**片方向にだけ確実な推論**を使う: 累計使用量が 200k を超えたなら、
/// その窓は物理的に 200k ではありえない (プロンプトが窓に収まらない)。
/// 収まる最小の段階まで繰り上げる。逆に「200k を超えていない = 200k の窓」
/// とは言えないので、そちらには推論しない。
const CONTEXT_WINDOW_TIERS: [u64; 2] = [200_000, 1_000_000];

/// 何も観測できていないときの既定 (最小の段階)。
/// 環境変数 `CCNEST_CONTEXT_WINDOW` で明示指定すれば推論より優先される。
pub const DEFAULT_CONTEXT_WINDOW: u64 = CONTEXT_WINDOW_TIERS[0];

/// 初回の逆走査で読む末尾バイト数。実測では目的の行は EOF から 3〜5 行
/// (2.8〜7.7KB) 以内に収まるので、通常はこの 1 回で足りる。
const TAIL_START: u64 = 64 * 1024;
/// 逆走査ウィンドウの上限。実測の最大行長が 5.1MB あるため、それを 1 行
/// 丸ごと収容できるところまでは倍々で広げる。超えたら全文読みに落ちる。
const TAIL_MAX: u64 = 8 * 1024 * 1024;

/// Claude Code がローカル生成する通知行 (API エラー / 上限到達 / 中断) の
/// model 値。実データに 21 行実在し、**セッション最終行がこれ**のケースも
/// あるため、モデル名としても usage としても採用してはいけない。
const SYNTHETIC_MODEL: &str = "<synthetic>";

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

/// セッション JSONL の 1 パス走査で得られる「今の状態」。
#[derive(Debug, Clone, Default)]
pub struct SessionInfo {
    pub usage: Option<ContextUsage>,
    /// 最後の非 synthetic な assistant 行の `message.model` (生の ID)。
    pub model: Option<String>,
    /// これまでに観測した最大の使用量。窓の推定に使う。単調増加なので、
    /// compact でコンテキストが縮んでも窓が 1M から 200k に戻ってしまう
    /// ことはない (`SessionTailer` が SessionInfo を持ち越すため)。
    peak_used: u64,
}

/// セッションファイルの読み取り状態。`(no session yet)` と実 I/O エラーを
/// 区別するために持つ (かつて `.ok()` で握り潰していたせいで、パス生成の
/// 恒久バグが「セッション未開始」に見えていた)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionState {
    /// まだファイルが無い (claude 起動直後 / shell ペイン)。
    #[default]
    NoSession,
    Ok,
    ReadError,
}

#[derive(Debug, Deserialize)]
struct Entry {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    message: Option<Message>,
}

#[derive(Debug, Deserialize)]
struct Message {
    #[serde(default)]
    usage: Option<Usage>,
    #[serde(default)]
    model: Option<String>,
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

impl Usage {
    fn total(&self) -> u64 {
        self.input_tokens.unwrap_or(0)
            + self.cache_creation_input_tokens.unwrap_or(0)
            + self.cache_read_input_tokens.unwrap_or(0)
            + self.output_tokens.unwrap_or(0)
    }
}

/// Translate a filesystem cwd into the encoded directory name that Claude Code
/// uses under `~/.claude/projects/`.
///
/// Rule (実データ 8/8 で検証): **英数字以外はすべて `-` に置換**する。
/// 非 ASCII はバイト数ではなく 1 文字につき `-` 1 個。
///
/// ```text
/// C:\Users\me\work\90_other\App  -> C--Users-me-work-90-other-App
/// C:\Users\me\work\50_ブログ     -> C--Users-me-work-50----
/// ```
pub fn encode_project_dir(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
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

/// 観測済みの最大使用量からコンテキストウィンドウを推定する。
/// 明示指定 (`CCNEST_CONTEXT_WINDOW`) があればそれを最優先。
fn window_for(peak_used: u64) -> u64 {
    if let Some(w) = std::env::var("CCNEST_CONTEXT_WINDOW")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|w| *w > 0)
    {
        return w;
    }
    CONTEXT_WINDOW_TIERS
        .iter()
        .copied()
        .find(|w| peak_used <= *w)
        .unwrap_or(CONTEXT_WINDOW_TIERS[CONTEXT_WINDOW_TIERS.len() - 1])
}

/// 1 行を取り込む。assistant 行のみ、かつ synthetic を除いて「最後が勝つ」。
fn apply_line(line: &str, info: &mut SessionInfo) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    let Ok(entry) = serde_json::from_str::<Entry>(line) else {
        return;
    };
    if entry.kind.as_deref() != Some("assistant") {
        return;
    }
    let Some(msg) = entry.message else {
        return;
    };
    if msg.model.as_deref() == Some(SYNTHETIC_MODEL) {
        return;
    }
    if let Some(m) = msg.model {
        info.model = Some(m);
    }
    if let Some(u) = msg.usage {
        let used = u.total();
        info.peak_used = info.peak_used.max(used);
        info.usage = Some(ContextUsage {
            used,
            window: window_for(info.peak_used),
        });
    }
}

pub fn session_info_from_str(content: &str) -> SessionInfo {
    let mut info = SessionInfo::default();
    for line in content.lines() {
        apply_line(line, &mut info);
    }
    info
}

/// ファイル末尾から `win` バイトを読み、**最初の改行より前を必ず捨てて**
/// 返す。バイト境界で切ってから行頭に揃えるので、UTF-8 のマルチバイト文字
/// を分断することが原理的に起きない。
///
/// 返り値は (行に揃ったテキスト, そのテキストが始まるファイル内オフセット)。
/// ファイル全体が `win` に収まる場合は先頭から返す (オフセット 0)。
fn read_tail(path: &Path, win: u64) -> Result<(String, u64)> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    let start = len.saturating_sub(win);
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    f.read_to_end(&mut buf)?;

    let (slice, offset) = if start == 0 {
        (&buf[..], 0)
    } else {
        match buf.iter().position(|b| *b == b'\n') {
            // 先頭の欠けた行を捨てる。改行の次から。
            Some(nl) => (&buf[nl + 1..], start + nl as u64 + 1),
            // ウィンドウ内に改行が 1 つも無い = 1 行の途中しか掴めていない。
            None => (&buf[buf.len()..], len),
        }
    };
    Ok((String::from_utf8_lossy(slice).into_owned(), offset))
}

/// 末尾から必要な行が見つかるまでウィンドウを倍々に広げて読む。
/// 見つからないまま `TAIL_MAX` を超えたら全文読みにフォールバックする。
fn read_tail_until_found(path: &Path) -> Result<(SessionInfo, u64)> {
    let file_len = File::open(path)?.metadata()?.len();
    let mut win = TAIL_START;
    loop {
        let (text, _) = read_tail(path, win)?;
        let info = session_info_from_str(&text);
        if info.model.is_some() || info.usage.is_some() || win >= file_len {
            return Ok((info, file_len));
        }
        if win >= TAIL_MAX {
            // 巨大行に阻まれている。ここまで来たら全文読みしかない。
            let mut buf = Vec::with_capacity(file_len as usize);
            File::open(path)?.read_to_end(&mut buf)?;
            let text = String::from_utf8_lossy(&buf);
            return Ok((session_info_from_str(&text), file_len));
        }
        win = win.saturating_mul(2);
    }
}

/// 1 ペイン分のセッション JSONL を追跡する。ペインと生死を共にするので
/// GC は不要。定常時のコストは `fs::metadata` 1 回だけ。
///
/// 初回のみ末尾から逆走査し、以降は前回の位置からの**差分バイトだけ**を
/// 読む。ファイルが縮んでいたら (rewind / 別セッションへの切替) 位置を
/// リセットして初回扱いに戻す。
#[derive(Debug, Default)]
pub struct SessionTailer {
    path: Option<PathBuf>,
    /// 消費済みバイト数。常に行境界に揃っている。
    offset: u64,
    stamp: Option<(u64, SystemTime)>,
    info: SessionInfo,
    state: SessionState,
}

impl SessionTailer {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            ..Default::default()
        }
    }

    pub fn info(&self) -> &SessionInfo {
        &self.info
    }

    pub fn state(&self) -> SessionState {
        self.state
    }

    /// 追跡を止めて状態を捨てる。ペインを shell に戻したときに呼ぶ。
    pub fn disable(&mut self) {
        *self = Self::default();
    }

    /// metadata が変化していれば読み直す。2 秒 tick から呼ぶ。
    pub fn refresh(&mut self) {
        let Some(path) = self.path.clone() else {
            self.state = SessionState::NoSession;
            return;
        };
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.state = SessionState::NoSession;
                return;
            }
            Err(_) => {
                self.state = SessionState::ReadError;
                return;
            }
        };
        let stamp = (
            meta.len(),
            meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        );
        if self.stamp == Some(stamp) {
            return;
        }

        // ファイルが縮んだ = 別ファイルに差し替わった。最初から読み直す。
        if meta.len() < self.offset {
            self.offset = 0;
            self.info = SessionInfo::default();
        }

        let result = if self.offset == 0 {
            read_tail_until_found(&path).map(|(info, len)| {
                self.info = info;
                len
            })
        } else {
            self.read_delta(&path, meta.len())
        };

        match result {
            Ok(new_offset) => {
                self.offset = new_offset;
                self.stamp = Some(stamp);
                self.state = SessionState::Ok;
            }
            Err(_) => self.state = SessionState::ReadError,
        }
    }

    /// 前回位置から末尾までの追記分だけを読む。末尾の未完了行 (書き込み途中)
    /// は消費せず、次回に持ち越す。
    fn read_delta(&mut self, path: &Path, len: u64) -> Result<u64> {
        let mut f = File::open(path)?;
        f.seek(SeekFrom::Start(self.offset))?;
        let mut buf = Vec::with_capacity((len - self.offset) as usize);
        f.read_to_end(&mut buf)?;

        let complete = match buf.iter().rposition(|b| *b == b'\n') {
            Some(nl) => nl + 1,
            None => return Ok(self.offset), // まだ 1 行も完結していない
        };
        let text = String::from_utf8_lossy(&buf[..complete]);
        for line in text.lines() {
            apply_line(line, &mut self.info);
        }
        Ok(self.offset + complete as u64)
    }
}

/// モデル ID を人間が読める短縮表記にする。
///
/// ```text
/// claude-opus-4-8           -> Opus 4.8
/// claude-fable-5            -> Fable 5
/// claude-haiku-4-5-20251001 -> Haiku 4.5
/// claude-opus-4-8[1m]       -> Opus 4.8 (1M)
/// (規則に当てはまらないもの)  -> 生の ID をそのまま
/// ```
pub fn pretty_model(id: &str) -> String {
    let (base, one_m) = match id.strip_suffix("[1m]") {
        Some(b) => (b, true),
        None => (id, false),
    };
    let Some(rest) = base.strip_prefix("claude-") else {
        return id.to_string();
    };
    let mut parts: Vec<&str> = rest.split('-').filter(|s| !s.is_empty()).collect();
    // 末尾の 8 桁日付 (例: -20251001) は表示しない。
    if parts
        .last()
        .is_some_and(|s| s.len() == 8 && s.chars().all(|c| c.is_ascii_digit()))
    {
        parts.pop();
    }
    let Some((family, version)) = parts.split_first() else {
        return id.to_string();
    };
    // バージョンが数値トークンだけで構成されていなければ規則の対象外。
    if version.is_empty()
        || !version
            .iter()
            .all(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
    {
        return id.to_string();
    }
    let mut out = String::with_capacity(base.len());
    let mut chars = family.chars();
    if let Some(first) = chars.next() {
        out.extend(first.to_uppercase());
        out.push_str(chars.as_str());
    }
    out.push(' ');
    out.push_str(&version.join("."));
    if one_m {
        out.push_str(" (1M)");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // ---- encode_project_dir ------------------------------------------------

    #[test]
    fn encodes_windows_path() {
        let p = PathBuf::from(r"C:\Users\me\proj");
        // ドライブのコロンも `-` になるので `C--` で始まる。
        assert_eq!(encode_project_dir(&p), "C--Users-me-proj");
    }

    #[test]
    fn encodes_unix_path() {
        let p = PathBuf::from("/home/me/proj");
        assert_eq!(encode_project_dir(&p), "-home-me-proj");
    }

    #[test]
    fn encodes_path_with_underscore_and_dot() {
        // 実データ: C--Users-mitam-Desktop-work-30-XTP2-POST-r5--docs-...
        let p = PathBuf::from(r"C:\work\30_XTP2_POST_r5\.docs");
        assert_eq!(encode_project_dir(&p), "C--work-30-XTP2-POST-r5--docs");
    }

    #[test]
    fn encodes_non_ascii_one_dash_per_char() {
        // 実データ: C--Users-mitam-Desktop-work-50----
        let p = PathBuf::from(r"C:\work\50_ブログ");
        assert_eq!(encode_project_dir(&p), "C--work-50----");
    }

    // ---- parsing -----------------------------------------------------------

    #[test]
    fn empty_jsonl_gives_no_usage() {
        let info = session_info_from_str("");
        assert!(info.usage.is_none());
        assert!(info.model.is_none());
    }

    #[test]
    fn usage_takes_last_message() {
        let jsonl = r#"
{"type":"user","message":{"role":"user"}}
{"type":"assistant","message":{"role":"assistant","model":"claude-opus-4-8","usage":{"input_tokens":1000,"cache_read_input_tokens":500,"output_tokens":200}}}
{"type":"assistant","message":{"role":"assistant","model":"claude-opus-4-8","usage":{"input_tokens":1500,"cache_read_input_tokens":800,"output_tokens":300}}}
"#;
        let info = session_info_from_str(jsonl);
        assert_eq!(info.usage.unwrap().used, 1500 + 800 + 300);
    }

    #[test]
    fn tolerates_malformed_lines() {
        let jsonl =
            "garbage\n{\"type\":\"assistant\",\"message\":{\"usage\":{\"input_tokens\":42}}}\n";
        let info = session_info_from_str(jsonl);
        assert_eq!(info.usage.unwrap().used, 42);
    }

    #[test]
    fn parses_model_from_last_assistant_line() {
        let jsonl = r#"
{"type":"assistant","message":{"model":"claude-fable-5","usage":{"input_tokens":10}}}
{"type":"assistant","message":{"model":"claude-opus-4-8","usage":{"input_tokens":20}}}
"#;
        assert_eq!(
            session_info_from_str(jsonl).model.as_deref(),
            Some("claude-opus-4-8")
        );
    }

    #[test]
    fn model_switch_mid_session_takes_latest() {
        // 実観測: 同一セッション内で fable-5 -> opus-4-8 に切り替わる。
        let jsonl = r#"
{"type":"assistant","message":{"model":"claude-fable-5","usage":{"input_tokens":10}}}
{"type":"user","message":{"role":"user","content":"<command-name>/model</command-name>"}}
{"type":"assistant","message":{"model":"claude-opus-4-8","usage":{"input_tokens":20}}}
"#;
        assert_eq!(
            session_info_from_str(jsonl).model.as_deref(),
            Some("claude-opus-4-8")
        );
    }

    #[test]
    fn skips_synthetic_model_and_takes_previous_real_one() {
        // 実データにセッション最終行が <synthetic> のケースがある。
        let jsonl = r#"
{"type":"assistant","message":{"model":"claude-opus-4-8","usage":{"input_tokens":1000,"output_tokens":50}}}
{"type":"assistant","message":{"model":"<synthetic>","id":"66cbae66-615f-4775-9cb4-8d31b8712f4e","usage":{"input_tokens":0,"output_tokens":0}}}
"#;
        let info = session_info_from_str(jsonl);
        assert_eq!(info.model.as_deref(), Some("claude-opus-4-8"));
        // synthetic の全ゼロ usage で上書きされていないこと (既存バグの回帰防止)。
        assert_eq!(info.usage.unwrap().used, 1050);
    }

    #[test]
    fn ignores_non_assistant_lines() {
        let jsonl = r#"
{"type":"user","message":{"model":"claude-opus-4-8","usage":{"input_tokens":999}}}
{"type":"mode","mode":"normal"}
"#;
        let info = session_info_from_str(jsonl);
        assert!(info.model.is_none());
        assert!(info.usage.is_none());
    }

    // ---- context window inference -----------------------------------------

    #[test]
    fn window_stays_at_first_tier_below_threshold() {
        let jsonl = r#"{"type":"assistant","message":{"model":"claude-opus-4-8","usage":{"input_tokens":150000}}}"#;
        let u = session_info_from_str(jsonl).usage.unwrap();
        assert_eq!(u.window, 200_000);
        assert_eq!(u.used, 150_000);
    }

    #[test]
    fn window_promotes_when_usage_exceeds_a_tier() {
        // 200k を超えた時点で「窓は 200k ではありえない」ことが確定する。
        let jsonl = r#"{"type":"assistant","message":{"model":"claude-opus-4-8","usage":{"input_tokens":237532}}}"#;
        let u = session_info_from_str(jsonl).usage.unwrap();
        assert_eq!(u.window, 1_000_000);
        // 100% に張り付かず、正しい比率になる。
        assert!((u.ratio() - 0.237_532).abs() < 0.001, "{}", u.ratio());
    }

    #[test]
    fn window_stays_promoted_after_compaction_drops_usage() {
        // compact でコンテキストが縮んでも窓は 1M のまま (peak が単調増加)。
        let jsonl = r#"
{"type":"assistant","message":{"model":"claude-opus-4-8","usage":{"input_tokens":344593}}}
{"type":"assistant","message":{"model":"claude-opus-4-8","usage":{"input_tokens":40000}}}
"#;
        let u = session_info_from_str(jsonl).usage.unwrap();
        assert_eq!(u.used, 40_000);
        assert_eq!(u.window, 1_000_000, "compact 後に 200k へ戻ってはいけない");
    }

    #[test]
    fn window_promotion_survives_incremental_refresh() {
        // 追記読みでも peak は持ち越される。
        let (_d, path) = write_tmp(
            "w.jsonl",
            "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-8\",\"usage\":{\"input_tokens\":344593}}}\n",
        );
        let mut t = SessionTailer::new(Some(path.clone()));
        t.refresh();
        assert_eq!(t.info().usage.unwrap().window, 1_000_000);

        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(b"{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-8\",\"usage\":{\"input_tokens\":50000}}}\n").unwrap();
        drop(f);
        t.refresh();
        let u = t.info().usage.unwrap();
        assert_eq!(u.used, 50_000);
        assert_eq!(u.window, 1_000_000);
    }

    #[test]
    fn window_caps_at_largest_tier() {
        let jsonl = r#"{"type":"assistant","message":{"model":"claude-opus-4-8","usage":{"input_tokens":9000000}}}"#;
        let u = session_info_from_str(jsonl).usage.unwrap();
        assert_eq!(u.window, 1_000_000);
        assert_eq!(u.ratio(), 1.0, "上限を超えたら 100% に張り付く");
    }

    #[test]
    fn model_none_when_no_assistant_line() {
        let jsonl = "{\"type\":\"user\",\"message\":{\"role\":\"user\"}}\n";
        assert!(session_info_from_str(jsonl).model.is_none());
    }

    // ---- tail read ---------------------------------------------------------

    fn write_tmp(name: &str, body: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        let mut f = File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        (dir, path)
    }

    #[test]
    fn tail_read_drops_partial_first_line() {
        let body = "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-fable-5\"}}\n\
                    {\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-8\"}}\n";
        let (_d, path) = write_tmp("s.jsonl", body);
        // 2 行目の途中から始まる窓を要求する。
        let win = 30;
        let (text, off) = read_tail(&path, win).unwrap();
        assert!(off > 0, "窓が先頭に届いてしまっている");
        // 欠けた行は捨てられ、完結した行だけが残る。
        for line in text.lines() {
            assert!(serde_json::from_str::<serde_json::Value>(line).is_ok());
        }
    }

    #[test]
    fn tail_read_keeps_utf8_intact_with_multibyte() {
        // 非 ASCII を大量に含む行をバイト境界で切っても壊れないこと。
        let mut body = String::new();
        for i in 0..50 {
            body.push_str(&format!(
                "{{\"type\":\"assistant\",\"message\":{{\"model\":\"claude-opus-4-8\",\"note\":\"日本語のテキスト{i}\",\"usage\":{{\"input_tokens\":{i}}}}}}}\n"
            ));
        }
        let (_d, path) = write_tmp("ja.jsonl", &body);
        let (text, _) = read_tail(&path, 100).unwrap();
        for line in text.lines() {
            assert!(serde_json::from_str::<serde_json::Value>(line).is_ok());
        }
    }

    #[test]
    fn tail_widens_window_when_first_window_has_no_assistant_line() {
        // 先頭に assistant 行、その後に巨大な非 assistant 行を置く。初期窓には
        // assistant 行が入らないので、ウィンドウが広がって初めて見つかる。
        let filler = "x".repeat(100 * 1024);
        let body = format!(
            "{{\"type\":\"assistant\",\"message\":{{\"model\":\"claude-opus-4-8\",\"usage\":{{\"input_tokens\":7}}}}}}\n\
             {{\"type\":\"user\",\"pad\":\"{filler}\"}}\n"
        );
        let (_d, path) = write_tmp("wide.jsonl", &body);
        let (info, len) = read_tail_until_found(&path).unwrap();
        assert_eq!(info.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(len, body.len() as u64);
    }

    #[test]
    fn tailer_reads_appended_lines_incrementally() {
        let (_d, path) = write_tmp(
            "t.jsonl",
            "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-fable-5\",\"usage\":{\"input_tokens\":10}}}\n",
        );
        let mut t = SessionTailer::new(Some(path.clone()));
        t.refresh();
        assert_eq!(t.state(), SessionState::Ok);
        assert_eq!(t.info().model.as_deref(), Some("claude-fable-5"));

        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(b"{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-8\",\"usage\":{\"input_tokens\":99}}}\n").unwrap();
        drop(f);

        t.refresh();
        assert_eq!(t.info().model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(t.info().usage.unwrap().used, 99);
    }

    #[test]
    fn tailer_holds_back_incomplete_trailing_line() {
        let (_d, path) = write_tmp(
            "p.jsonl",
            "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-fable-5\"}}\n",
        );
        let mut t = SessionTailer::new(Some(path.clone()));
        t.refresh();

        // 改行の無い書きかけの行を追記する。
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(b"{\"type\":\"assistant\",\"message\":{\"model\":\"claude-op")
            .unwrap();
        drop(f);
        t.refresh();
        assert_eq!(t.info().model.as_deref(), Some("claude-fable-5"));

        // 続きが書かれて行が完結したら取り込まれる。
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(b"us-4-8\"}}\n").unwrap();
        drop(f);
        t.refresh();
        assert_eq!(t.info().model.as_deref(), Some("claude-opus-4-8"));
    }

    #[test]
    fn tailer_reports_no_session_for_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = SessionTailer::new(Some(dir.path().join("nope.jsonl")));
        t.refresh();
        assert_eq!(t.state(), SessionState::NoSession);
        assert!(t.info().model.is_none());
    }

    #[test]
    fn tailer_resets_when_file_shrinks() {
        let (_d, path) = write_tmp(
            "r.jsonl",
            "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-8\"}}\n",
        );
        let mut t = SessionTailer::new(Some(path.clone()));
        t.refresh();
        assert_eq!(t.info().model.as_deref(), Some("claude-opus-4-8"));

        // より短い内容で置き換える。
        std::fs::write(
            &path,
            "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-fable-5\"}}\n",
        )
        .unwrap();
        t.refresh();
        assert_eq!(t.info().model.as_deref(), Some("claude-fable-5"));
    }

    #[test]
    fn tailer_disable_clears_state() {
        let (_d, path) = write_tmp(
            "d.jsonl",
            "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-8\"}}\n",
        );
        let mut t = SessionTailer::new(Some(path));
        t.refresh();
        assert!(t.info().model.is_some());
        t.disable();
        assert!(t.info().model.is_none());
        assert_eq!(t.state(), SessionState::NoSession);
    }

    // ---- pretty_model ------------------------------------------------------

    #[test]
    fn pretty_model_known_ids() {
        assert_eq!(pretty_model("claude-opus-4-8"), "Opus 4.8");
        assert_eq!(pretty_model("claude-fable-5"), "Fable 5");
        assert_eq!(pretty_model("claude-sonnet-5"), "Sonnet 5");
    }

    #[test]
    fn pretty_model_strips_date_suffix() {
        assert_eq!(pretty_model("claude-haiku-4-5-20251001"), "Haiku 4.5");
    }

    #[test]
    fn pretty_model_1m_suffix() {
        assert_eq!(pretty_model("claude-opus-4-8[1m]"), "Opus 4.8 (1M)");
    }

    #[test]
    fn pretty_model_unknown_passthrough() {
        assert_eq!(pretty_model("gpt-foo"), "gpt-foo");
        assert_eq!(pretty_model("claude-unknown-x"), "claude-unknown-x");
        assert_eq!(pretty_model("claude-opus"), "claude-opus");
        assert_eq!(pretty_model("<synthetic>"), "<synthetic>");
    }
}

// Version History
// ver0.1 - 2026-04-25 - Initial JSONL context-usage parser.
// ver0.2 - 2026-07-20 - Fixed encode_project_dir (non-alphanumeric -> '-'; the
//                       old rule never matched a real ~/.claude/projects dir, so
//                       context usage silently showed "(no session yet)").
//                       Added model extraction (<synthetic> excluded), byte-based
//                       incremental tail reading (SessionTailer) to remove
//                       multi-MB full-file parses from the render path, and
//                       pretty_model() for short display names.
// ver0.3 - 2026-07-20 - Infer the context window instead of hardcoding 200k.
//                       Nothing on disk states a session's window (the transcript
//                       strips the `[1m]` model suffix), but usage above a tier
//                       proves the window is larger — a prompt cannot exceed its
//                       own window. Peak usage is monotonic so compaction never
//                       demotes the window back. CCNEST_CONTEXT_WINDOW still wins.
