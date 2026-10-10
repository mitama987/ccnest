//! Claude のプラン利用制限 (5 時間のセッション枠 / 週の全モデル枠 / モデル別の
//! 週枠) を取りに行き、ステータスバーに出す文字列を作る。
//!
//! 取得元は Claude Code の `/usage` と同じ `GET /api/oauth/usage`。**非公式・
//! ドキュメント無し** なので、形が変わったら黙って表示を消す (ccnest 本体の
//! 動作には一切影響させない)。公式のステータスライン (`rate_limits`) は
//! 5h / 週しか持たずモデル別の週枠 (例: Fable) が無いうえ、設定すると Claude
//! Code が `esc to interrupt` を出さなくなり `detect_status` が壊れるため使わない。
//!
//! - 認証は Claude Code がログイン時に保存した `~/.claude/.credentials.json` の
//!   アクセストークンを読むだけ。**トークンの更新 (refresh) は絶対にしない**:
//!   refresh token が入れ替わって Claude Code 側のログインが壊れる。期限切れなら
//!   Claude Code が更新するのを待つ。
//! - 問い合わせは専用スレッド ([`spawn_usage_poller`]) で行い、結果は
//!   [`UsageCell`] に置く。イベントループは 2 秒 tick でそれをコピーするだけ。
//! - ccnest を何窓開いても問い合わせが全体で間隔 (既定 5 分) に 1 回で済むよう、
//!   結果を `%APPDATA%\ccnest\usage-cache.json` で共有する。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `/usage` が叩いているエンドポイント。
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
/// OAuth トークンで API を叩くときに必要なベータヘッダ。
const OAUTH_BETA: &str = "oauth-2025-04-20";
/// 1 回の問い合わせの上限 (Claude Code 自身も 5 秒)。
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
/// 問い合わせ間隔の既定値と下限 (`CCNEST_USAGE_POLL_SECS` で変更可)。
const DEFAULT_INTERVAL: Duration = Duration::from_secs(300);
const MIN_INTERVAL_SECS: u64 = 60;
/// 429 を返されたら、この間は問い合わせない。
const RATE_LIMITED_BACKOFF: Duration = Duration::from_secs(15 * 60);
/// トークンが無い/期限切れのときの再確認間隔 (Claude Code の更新待ち)。
const NO_TOKEN_RETRY: Duration = Duration::from_secs(60);
/// 最後の成功からこれ以上経った値は表示しない (古い数字を出し続けない)。
pub const STALE_AFTER: Duration = Duration::from_secs(30 * 60);
/// トークンの期限ぎりぎりで投げて 401 にならないための余裕。
const EXPIRY_MARGIN_MS: i64 = 60_000;

/// 1 つの制限枠。`pct` は使用率 0〜100。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bucket {
    pub pct: f64,
    pub resets_at: Option<DateTime<Utc>>,
}

/// 1 回の問い合わせ結果。`models` はモデル別の週枠 (表示名, 枠)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageSnapshot {
    pub five_hour: Option<Bucket>,
    pub seven_day: Option<Bucket>,
    pub models: Vec<(String, Bucket)>,
    pub fetched_at: DateTime<Utc>,
}

/// ポーラースレッドとイベントループで分け持つ最新値。
pub type UsageCell = Arc<Mutex<Option<UsageSnapshot>>>;

/// 表示色の段階 (使用率の最大値で決める)。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UsageLevel {
    #[default]
    Normal,
    /// 70% 以上。
    Warn,
    /// 90% 以上。
    Crit,
}

/// `/api/oauth/usage` の応答を読む純粋関数。
///
/// - 5h / 週は `five_hour` / `seven_day` の `utilization` (0〜100)。無ければ
///   `limits[]` の `kind: "session"` / `"weekly_all"` の `percent` で補う。
/// - モデル別の週枠は `limits[]` のうち `scope.model.display_name` を持つ行
///   (2026-10 時点では `kind: "weekly_scoped"`, `display_name: "Fable"`)。
///   `/usage` の「Current week (Fable)」と同じもの。同名は先勝ち。
/// - 何も取れなければ None (= 表示しない)。未知のフィールドは無視する。
pub fn parse_usage(body: &str, fetched_at: DateTime<Utc>) -> Option<UsageSnapshot> {
    let v: Value = serde_json::from_str(body).ok()?;
    let limits: &[Value] = v
        .get("limits")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let unscoped = |kind: &str| {
        limits
            .iter()
            .filter(|row| model_name(row).is_none())
            .find(|row| row.get("kind").and_then(Value::as_str) == Some(kind))
            .and_then(bucket_from_limit)
    };
    let five_hour = bucket_from_window(v.get("five_hour")).or_else(|| unscoped("session"));
    let seven_day = bucket_from_window(v.get("seven_day")).or_else(|| unscoped("weekly_all"));

    let mut models: Vec<(String, Bucket)> = Vec::new();
    for row in limits {
        let Some(name) = model_name(row) else {
            continue;
        };
        if models.iter().any(|(n, _)| n == name) {
            continue;
        }
        if let Some(b) = bucket_from_limit(row) {
            models.push((name.to_string(), b));
        }
    }

    if five_hour.is_none() && seven_day.is_none() && models.is_empty() {
        return None;
    }
    Some(UsageSnapshot {
        five_hour,
        seven_day,
        models,
        fetched_at,
    })
}

fn model_name(row: &Value) -> Option<&str> {
    row.pointer("/scope/model/display_name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn bucket_from_window(w: Option<&Value>) -> Option<Bucket> {
    let w = w?;
    Some(Bucket {
        pct: w.get("utilization")?.as_f64()?,
        resets_at: parse_time(w.get("resets_at")),
    })
}

fn bucket_from_limit(row: &Value) -> Option<Bucket> {
    Some(Bucket {
        pct: row.get("percent")?.as_f64()?,
        resets_at: parse_time(row.get("resets_at")),
    })
}

fn parse_time(v: Option<&Value>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(v?.as_str()?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// 表示する使用率 (整数 %)。`/usage` と同じく切り捨て。リセット時刻を
/// 過ぎた枠は、次の問い合わせを待たずに 0% とみなす。
fn effective_pct(b: &Bucket, now: DateTime<Utc>) -> u32 {
    if b.resets_at.is_some_and(|t| t <= now) {
        return 0;
    }
    b.pct.clamp(0.0, 100.0).floor() as u32
}

/// 表示順 (5h → wk → モデル別) に並べた枠。
fn buckets(s: &UsageSnapshot) -> Vec<(&str, &Bucket)> {
    let mut out: Vec<(&str, &Bucket)> = Vec::new();
    if let Some(b) = &s.five_hour {
        out.push(("5h", b));
    }
    if let Some(b) = &s.seven_day {
        out.push(("wk", b));
    }
    for (name, b) in &s.models {
        out.push((name.as_str(), b));
    }
    out
}

/// ステータスバーに出す文字列: `5h 42% · wk 18% · Fable 7%`。
pub fn format_usage(s: &UsageSnapshot, now: DateTime<Utc>) -> String {
    buckets(s)
        .into_iter()
        .map(|(name, b)| format!("{name} {}%", effective_pct(b, now)))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// 解除までの残り時間の前に置く記号。Windows Terminal は絵文字 (2 桁) で描き、
/// unicode-width の申告も 2 なので行幅の計算と食い違わない。
pub const HOURGLASS: &str = "⏳";
/// 隣り合う枠の解除時刻がこの秒数以内なら同じ時刻とみなして 1 回にまとめる
/// (実応答では wk と Fable が秒以下だけ違う)。
const SAME_RESET_TOLERANCE_SECS: i64 = 60;

/// 解除までの残り時間。解除済み (`now >= reset`) は None。
///
/// - 1 時間未満: `{m}m` (切り上げ。`0m` は出さず最小 `1m`)
/// - 1 日未満: `{h}h{m}m` (分は切り捨て・ゼロ埋めなし)
/// - 1 日以上: `{d}d{h}h` (分は捨てる)
pub fn format_remaining(reset: DateTime<Utc>, now: DateTime<Utc>) -> Option<String> {
    let secs = (reset - now).num_seconds();
    if secs <= 0 {
        return None;
    }
    if secs < 3600 {
        return Some(format!("{}m", ((secs + 59) / 60).max(1)));
    }
    let mins = secs / 60;
    let hours = mins / 60;
    if hours < 24 {
        return Some(format!("{hours}h{}m", mins % 60));
    }
    Some(format!("{}d{}h", hours / 24, hours % 24))
}

/// まだ先の解除時刻 (None・解除済みは None)。
fn pending_reset(b: &Bucket, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    b.resets_at.filter(|t| *t > now)
}

/// ステータスバーに出す文字列 (残り時間つき):
/// `5h 42% ⏳2h10m · wk 18% · Fable 7% ⏳2d9h`。
///
/// 隣り合う枠の解除時刻が [`SAME_RESET_TOLERANCE_SECS`] 以内なら 1 つのグループに
/// して、末尾に 1 回だけ ⏳ を付ける (wk と Fable は普段同じ時刻)。解除時刻が
/// 無い・解除済みの枠は % だけで、グループにも入らない。
pub fn format_usage_with_reset(s: &UsageSnapshot, now: DateTime<Utc>) -> String {
    let buckets = buckets(s);
    let mut groups: Vec<String> = Vec::new();
    let mut i = 0;
    while i < buckets.len() {
        let (name, b) = buckets[i];
        let mut names = vec![format!("{name} {}%", effective_pct(b, now))];
        let reset = pending_reset(b, now);
        let mut j = i + 1;
        if let Some(r) = reset {
            while j < buckets.len() {
                let (next_name, next) = buckets[j];
                let same = pending_reset(next, now)
                    .is_some_and(|r2| (r2 - r).num_seconds().abs() <= SAME_RESET_TOLERANCE_SECS);
                if !same {
                    break;
                }
                names.push(format!("{next_name} {}%", effective_pct(next, now)));
                j += 1;
            }
        }
        let mut text = names.join(" · ");
        if let Some(rem) = reset.and_then(|r| format_remaining(r, now)) {
            text.push_str(&format!(" {HOURGLASS}{rem}"));
        }
        groups.push(text);
        i = j;
    }
    groups.join(" · ")
}

/// 一番使っている枠で色の段階を決める。
pub fn usage_level(s: &UsageSnapshot, now: DateTime<Utc>) -> UsageLevel {
    let max = buckets(s)
        .into_iter()
        .map(|(_, b)| effective_pct(b, now))
        .max()
        .unwrap_or(0);
    if max >= 90 {
        UsageLevel::Crit
    } else if max >= 70 {
        UsageLevel::Warn
    } else {
        UsageLevel::Normal
    }
}

/// ステータスバーに出す利用制限。`full` は残り時間つき、`bare` は % だけ
/// (幅が足りないときの代替)。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageLabel {
    pub full: String,
    pub bare: String,
    pub level: UsageLevel,
}

/// 描画用: 古すぎる値・空の値は None (セグメントごと出さない)。
pub fn status_label(s: &UsageSnapshot, now: DateTime<Utc>) -> Option<UsageLabel> {
    if !is_younger_than(s, now, STALE_AFTER) {
        return None;
    }
    let bare = format_usage(s, now);
    if bare.is_empty() {
        return None;
    }
    Some(UsageLabel {
        full: format_usage_with_reset(s, now),
        bare,
        level: usage_level(s, now),
    })
}

/// `fetched_at` から `max_age` 未満か。未来の時刻 (時計ずれ) は新しいとみなす。
fn is_younger_than(s: &UsageSnapshot, now: DateTime<Utc>, max_age: Duration) -> bool {
    match (now - s.fetched_at).to_std() {
        Ok(age) => age < max_age,
        Err(_) => true,
    }
}

/// `.credentials.json` の場所。Claude Code と同じく `CLAUDE_CONFIG_DIR` があれば
/// そこ、無ければ `~/.claude`。env 読みは注入 (テストで env を変異させない)。
pub fn credentials_path(
    get: impl Fn(&str) -> Option<String>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(dir) = get("CLAUDE_CONFIG_DIR") {
        let dir = dir.trim();
        if !dir.is_empty() {
            return Some(PathBuf::from(dir).join(".credentials.json"));
        }
    }
    Some(home?.join(".claude").join(".credentials.json"))
}

/// `.credentials.json` の中身からアクセストークンを取り出す。期限切れ
/// (余裕 1 分) なら None。ここでは更新しない (モジュール冒頭の注意を参照)。
pub fn access_token_from(json: &str, now_ms: i64) -> Option<String> {
    let v: Value = serde_json::from_str(json).ok()?;
    let oauth = v.get("claudeAiOauth")?;
    let token = oauth.get("accessToken")?.as_str()?.trim();
    if token.is_empty() {
        return None;
    }
    if let Some(exp) = oauth.get("expiresAt").and_then(Value::as_i64) {
        if exp <= now_ms + EXPIRY_MARGIN_MS {
            return None;
        }
    }
    Some(token.to_string())
}

/// ポーラーの設定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollConfig {
    pub enabled: bool,
    pub interval: Duration,
}

/// - `CCNEST_USAGE=off` (大文字小文字無視) → 機能ごと無効 (問い合わせもしない)。
/// - `CCNEST_USAGE_POLL_SECS` → 問い合わせ間隔 (秒)。60 未満は 60。不正値は既定 300。
pub fn poll_config_from(get: impl Fn(&str) -> Option<String>) -> PollConfig {
    let enabled = !get("CCNEST_USAGE").is_some_and(|v| v.trim().eq_ignore_ascii_case("off"));
    let interval = get("CCNEST_USAGE_POLL_SECS")
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|s| Duration::from_secs(s.max(MIN_INTERVAL_SECS)))
        .unwrap_or(DEFAULT_INTERVAL);
    PollConfig { enabled, interval }
}

/// 問い合わせの失敗理由 (どれも黙って前回値を保持する。違うのは次の待ち時間だけ)。
#[derive(Debug, Clone, PartialEq, Eq)]
enum FetchError {
    /// トークンが無い/期限切れ (macOS はキーチェーン保存なのでここに来る)。
    NoToken,
    /// 429。
    RateLimited,
    /// その他の HTTP ステータス (401 等)。
    Http(u16),
    /// 接続・TLS・タイムアウト等。
    Transport,
    /// 応答の形が想定外 (非公式 API なので変わりうる)。
    BadBody,
}

/// 失敗後、次に問い合わせるまでの待ち。
fn retry_after(err: &FetchError, interval: Duration) -> Duration {
    match err {
        FetchError::NoToken => NO_TOKEN_RETRY.min(interval),
        FetchError::RateLimited => RATE_LIMITED_BACKOFF.max(interval),
        FetchError::Http(_) | FetchError::Transport | FetchError::BadBody => interval,
    }
}

/// 共有キャッシュがまだ使える (= 自分で問い合わせなくてよい) なら、
/// 次に見に行くまでの残り時間を返す。
fn cache_wait(cached: &UsageSnapshot, now: DateTime<Utc>, interval: Duration) -> Option<Duration> {
    let age = (now - cached.fetched_at).to_std().unwrap_or(Duration::ZERO);
    if age >= interval {
        return None;
    }
    Some((interval - age).max(Duration::from_secs(1)))
}

fn cache_path() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("ccnest").join("usage-cache.json"))
}

fn read_cache(path: &Path) -> Option<UsageSnapshot> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// 一時ファイルに書いてから rename (他の窓が書きかけを読まないように)。
fn write_cache(path: &Path, snap: &UsageSnapshot) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(snap)?)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

fn read_token() -> Result<String, FetchError> {
    let path =
        credentials_path(|k| std::env::var(k).ok(), dirs::home_dir()).ok_or(FetchError::NoToken)?;
    let json = std::fs::read_to_string(path).map_err(|_| FetchError::NoToken)?;
    access_token_from(&json, Utc::now().timestamp_millis()).ok_or(FetchError::NoToken)
}

fn fetch(token: &str) -> Result<UsageSnapshot, FetchError> {
    let agent = ureq::AgentBuilder::new().timeout(FETCH_TIMEOUT).build();
    let resp = agent
        .get(USAGE_URL)
        .set("Authorization", &format!("Bearer {token}"))
        .set("anthropic-beta", OAUTH_BETA)
        .set("User-Agent", concat!("ccnest/", env!("CARGO_PKG_VERSION")))
        .call();
    let body = match resp {
        Ok(r) => r.into_string().map_err(|_| FetchError::Transport)?,
        Err(ureq::Error::Status(429, _)) => return Err(FetchError::RateLimited),
        Err(ureq::Error::Status(code, _)) => return Err(FetchError::Http(code)),
        Err(ureq::Error::Transport(_)) => return Err(FetchError::Transport),
    };
    parse_usage(&body, Utc::now()).ok_or(FetchError::BadBody)
}

fn store(cell: &UsageCell, snap: UsageSnapshot) {
    if let Ok(mut slot) = cell.lock() {
        *slot = Some(snap);
    }
}

/// 1 回ぶんの「キャッシュを見る → 古ければ問い合わせる」。次に起きるまでの
/// 待ち時間を返す。
fn poll_once(cell: &UsageCell, interval: Duration) -> Duration {
    let cache = cache_path();
    if let Some(cached) = cache.as_deref().and_then(read_cache) {
        let wait = cache_wait(&cached, Utc::now(), interval);
        // 間隔を過ぎたキャッシュでも、問い合わせが失敗したときの表示用に置いておく
        // (30 分を過ぎたら status_label が隠す)。
        store(cell, cached);
        if let Some(wait) = wait {
            return wait;
        }
    }
    match read_token().and_then(|t| fetch(&t)) {
        Ok(snap) => {
            if let Some(path) = cache.as_deref() {
                let _ = write_cache(path, &snap);
            }
            store(cell, snap);
            interval
        }
        Err(e) => retry_after(&e, interval),
    }
}

/// 利用制限の問い合わせスレッドを起動する。`CCNEST_USAGE=off` なら何もしない。
/// 終了は考えない: メインが return → プロセス終了でこのスレッドも消える。
pub fn spawn_usage_poller(cell: UsageCell) {
    let cfg = poll_config_from(|k| std::env::var(k).ok());
    if !cfg.enabled {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("ccnest-usage".to_string())
        .spawn(move || loop {
            let wait = poll_once(&cell, cfg.interval);
            std::thread::sleep(wait);
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// 2026-10-06 に実際の `/api/oauth/usage` から取った応答 (金額系の行は省略)。
    const FIXTURE: &str = r#"{"five_hour":{"utilization":1.0,"resets_at":"2026-10-06T14:40:00.254067+00:00","limit_dollars":null,"used_dollars":null,"remaining_dollars":null,"locked_reason":null},"seven_day":{"utilization":0.0,"resets_at":"2026-10-12T18:00:00.254094+00:00","limit_dollars":null,"used_dollars":null,"remaining_dollars":null,"locked_reason":null},"seven_day_oauth_apps":null,"seven_day_opus":null,"seven_day_sonnet":null,"seven_day_cowork":null,"seven_day_omelette":null,"limits":[{"kind":"session","group":"session","percent":1,"severity":"normal","resets_at":"2026-10-06T14:40:00.254067+00:00","scope":null,"is_active":true},{"kind":"weekly_all","group":"weekly","percent":0,"severity":"normal","resets_at":"2026-10-12T18:00:00.254094+00:00","scope":null,"is_active":false},{"kind":"weekly_scoped","group":"weekly","percent":0,"severity":"normal","resets_at":"2026-10-12T18:00:00+00:00","scope":{"model":{"id":null,"display_name":"Fable"},"surface":null},"is_active":false}],"member_dashboard_available":false,"seven_day_breakdown":null}"#;

    fn t(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 6, h, m, 0).unwrap()
    }

    fn bucket(pct: f64) -> Bucket {
        Bucket {
            pct,
            resets_at: None,
        }
    }

    fn snap(five: Option<f64>, week: Option<f64>, models: &[(&str, f64)]) -> UsageSnapshot {
        UsageSnapshot {
            five_hour: five.map(bucket),
            seven_day: week.map(bucket),
            models: models
                .iter()
                .map(|(n, p)| (n.to_string(), bucket(*p)))
                .collect(),
            fetched_at: t(10, 0),
        }
    }

    // ---- parse --------------------------------------------------------------

    #[test]
    fn parse_reads_real_response() {
        // 実物の応答から 5h / 週 / Fable の 3 つが取れる。
        let s = parse_usage(FIXTURE, t(10, 0)).unwrap();
        assert_eq!(s.five_hour.as_ref().unwrap().pct, 1.0);
        assert_eq!(
            s.five_hour.as_ref().unwrap().resets_at,
            Some(
                DateTime::parse_from_rfc3339("2026-10-06T14:40:00.254067+00:00")
                    .unwrap()
                    .with_timezone(&Utc)
            )
        );
        assert_eq!(s.seven_day.as_ref().unwrap().pct, 0.0);
        assert_eq!(s.models.len(), 1);
        assert_eq!(s.models[0].0, "Fable");
        assert_eq!(s.models[0].1.pct, 0.0);
        assert_eq!(s.fetched_at, t(10, 0));
    }

    #[test]
    fn parse_falls_back_to_limits_rows() {
        // five_hour / seven_day が null でも limits[] の session / weekly_all で補う。
        let body = r#"{"five_hour":null,"seven_day":null,"limits":[
            {"kind":"session","percent":42,"resets_at":null,"scope":null},
            {"kind":"weekly_all","percent":18,"resets_at":null,"scope":null}]}"#;
        let s = parse_usage(body, t(10, 0)).unwrap();
        assert_eq!(s.five_hour.unwrap().pct, 42.0);
        assert_eq!(s.seven_day.unwrap().pct, 18.0);
        assert!(s.models.is_empty());
    }

    #[test]
    fn parse_keeps_first_row_per_model_name() {
        // 同じモデル名の行が複数あっても 1 つだけ (先勝ち)。空の名前は無視。
        let body = r#"{"limits":[
            {"kind":"weekly_scoped","percent":7,"scope":{"model":{"display_name":"Fable"}}},
            {"kind":"other","percent":99,"scope":{"model":{"display_name":"Fable"}}},
            {"kind":"weekly_scoped","percent":5,"scope":{"model":{"display_name":"  "}}}]}"#;
        let s = parse_usage(body, t(10, 0)).unwrap();
        assert_eq!(s.models.len(), 1);
        assert_eq!(s.models[0], ("Fable".to_string(), bucket(7.0)));
    }

    #[test]
    fn parse_returns_none_when_nothing_is_usable() {
        // 想定外の形・空・JSON でないものは None (表示しない)。
        assert!(parse_usage("{}", t(10, 0)).is_none());
        assert!(parse_usage(r#"{"five_hour":{"utilization":null}}"#, t(10, 0)).is_none());
        assert!(parse_usage("not json", t(10, 0)).is_none());
        assert!(parse_usage("[]", t(10, 0)).is_none());
    }

    // ---- format / level -----------------------------------------------------

    #[test]
    fn format_shows_all_three_with_floor() {
        // /usage と同じく切り捨て。
        let s = snap(Some(42.9), Some(18.0), &[("Fable", 7.5)]);
        assert_eq!(format_usage(&s, t(10, 0)), "5h 42% · wk 18% · Fable 7%");
    }

    #[test]
    fn format_treats_passed_reset_as_zero() {
        // リセット時刻を過ぎた枠は次の問い合わせを待たずに 0%。
        let mut s = snap(Some(55.0), Some(30.0), &[]);
        s.five_hour.as_mut().unwrap().resets_at = Some(t(11, 0));
        assert_eq!(format_usage(&s, t(10, 59)), "5h 55% · wk 30%");
        assert_eq!(format_usage(&s, t(11, 0)), "5h 0% · wk 30%");
    }

    #[test]
    fn format_clamps_out_of_range_values() {
        let s = snap(Some(130.0), Some(-3.0), &[]);
        assert_eq!(format_usage(&s, t(10, 0)), "5h 100% · wk 0%");
    }

    #[test]
    fn level_uses_the_highest_bucket() {
        assert_eq!(
            usage_level(&snap(Some(69.9), Some(10.0), &[]), t(10, 0)),
            UsageLevel::Normal
        );
        assert_eq!(
            usage_level(&snap(Some(1.0), Some(70.0), &[]), t(10, 0)),
            UsageLevel::Warn
        );
        assert_eq!(
            usage_level(&snap(Some(1.0), None, &[("Fable", 90.0)]), t(10, 0)),
            UsageLevel::Crit
        );
        assert_eq!(
            usage_level(&snap(None, None, &[]), t(10, 0)),
            UsageLevel::Normal
        );
    }

    #[test]
    fn status_label_hides_stale_values() {
        // 最後の成功から 30 分以上経った値は出さない。
        let s = snap(Some(42.0), Some(18.0), &[("Fable", 7.0)]);
        assert_eq!(
            status_label(&s, t(10, 29)),
            Some(UsageLabel {
                full: "5h 42% · wk 18% · Fable 7%".to_string(),
                bare: "5h 42% · wk 18% · Fable 7%".to_string(),
                level: UsageLevel::Normal,
            })
        );
        assert_eq!(status_label(&s, t(10, 30)), None);
        // 時計ずれで fetched_at が未来でも出す。
        assert!(status_label(&s, t(9, 50)).is_some());
    }

    // ---- remaining time / reset grouping -------------------------------------

    /// `t(10, 0)` から秒数ぶん先の時刻。
    fn after(secs: i64) -> DateTime<Utc> {
        t(10, 0) + chrono::Duration::seconds(secs)
    }

    #[test]
    fn remaining_formats_each_range() {
        let now = t(10, 0);
        // 1 時間未満は分 (切り上げ)。0 にはしない。
        assert_eq!(format_remaining(after(1), now), Some("1m".to_string()));
        assert_eq!(format_remaining(after(60), now), Some("1m".to_string()));
        assert_eq!(format_remaining(after(61), now), Some("2m".to_string()));
        assert_eq!(
            format_remaining(after(59 * 60), now),
            Some("59m".to_string())
        );
        // 1 日未満は 時h分m (分は切り捨て、ゼロ埋めなし)。
        assert_eq!(format_remaining(after(3600), now), Some("1h0m".to_string()));
        assert_eq!(
            format_remaining(after(2 * 3600 + 10 * 60 + 59), now),
            Some("2h10m".to_string())
        );
        assert_eq!(
            format_remaining(after(23 * 3600 + 59 * 60), now),
            Some("23h59m".to_string())
        );
        // 1 日以上は 日d時h (分は捨てる)。
        assert_eq!(
            format_remaining(after(86_400), now),
            Some("1d0h".to_string())
        );
        assert_eq!(
            format_remaining(after(2 * 86_400 + 9 * 3600 + 30 * 60), now),
            Some("2d9h".to_string())
        );
        // 解除済み (ちょうど・過去) は出さない。
        assert_eq!(format_remaining(now, now), None);
        assert_eq!(format_remaining(after(-1), now), None);
    }

    #[test]
    fn with_reset_appends_remaining_per_bucket() {
        // 解除時刻が違う枠は個別に ⏳ が付く。
        let mut s = snap(Some(42.0), Some(18.0), &[("Fable", 7.0)]);
        s.five_hour.as_mut().unwrap().resets_at = Some(after(2 * 3600 + 10 * 60));
        s.seven_day.as_mut().unwrap().resets_at = Some(after(2 * 86_400 + 9 * 3600));
        s.models[0].1.resets_at = Some(after(4 * 86_400 + 3600));
        assert_eq!(
            format_usage_with_reset(&s, t(10, 0)),
            "5h 42% ⏳2h10m · wk 18% ⏳2d9h · Fable 7% ⏳4d1h"
        );
        // % だけの表示はそのまま。
        assert_eq!(format_usage(&s, t(10, 0)), "5h 42% · wk 18% · Fable 7%");
    }

    #[test]
    fn with_reset_groups_adjacent_buckets_within_a_minute() {
        // wk と Fable の解除が 60 秒以内なら、末尾に 1 回だけ。
        let mut s = snap(Some(42.0), Some(18.0), &[("Fable", 7.0)]);
        s.five_hour.as_mut().unwrap().resets_at = Some(after(2 * 3600 + 10 * 60));
        s.seven_day.as_mut().unwrap().resets_at = Some(after(2 * 86_400 + 9 * 3600 + 1));
        s.models[0].1.resets_at = Some(after(2 * 86_400 + 9 * 3600 + 60));
        assert_eq!(
            format_usage_with_reset(&s, t(10, 0)),
            "5h 42% ⏳2h10m · wk 18% · Fable 7% ⏳2d9h"
        );
        // 61 秒離れたら別々。
        s.models[0].1.resets_at = Some(after(2 * 86_400 + 9 * 3600 + 62));
        assert_eq!(
            format_usage_with_reset(&s, t(10, 0)),
            "5h 42% ⏳2h10m · wk 18% ⏳2d9h · Fable 7% ⏳2d9h"
        );
    }

    #[test]
    fn with_reset_groups_only_adjacent_buckets() {
        // 隣接していれば 5h と wk も同じ規則でまとまる (一般則)。
        // 隣接していない 5h と Fable は、同じ時刻でもまとまらない。
        let mut s = snap(Some(42.0), Some(18.0), &[("Fable", 7.0)]);
        s.five_hour.as_mut().unwrap().resets_at = Some(after(3600));
        s.seven_day.as_mut().unwrap().resets_at = Some(after(3600));
        s.models[0].1.resets_at = Some(after(2 * 86_400));
        assert_eq!(
            format_usage_with_reset(&s, t(10, 0)),
            "5h 42% · wk 18% ⏳1h0m · Fable 7% ⏳2d0h"
        );
        s.seven_day.as_mut().unwrap().resets_at = Some(after(2 * 86_400));
        s.models[0].1.resets_at = Some(after(3600));
        assert_eq!(
            format_usage_with_reset(&s, t(10, 0)),
            "5h 42% ⏳1h0m · wk 18% ⏳2d0h · Fable 7% ⏳1h0m"
        );
    }

    #[test]
    fn with_reset_omits_remaining_when_unknown_or_passed() {
        // resets_at が無い / 解除済みの枠は % だけ (0% 扱いは format_usage と同じ)。
        let mut s = snap(Some(55.0), Some(30.0), &[("Fable", 7.0)]);
        s.five_hour.as_mut().unwrap().resets_at = Some(t(11, 0));
        s.models[0].1.resets_at = Some(after(3 * 86_400));
        assert_eq!(
            format_usage_with_reset(&s, t(10, 59)),
            "5h 55% ⏳1m · wk 30% · Fable 7% ⏳2d23h"
        );
        assert_eq!(
            format_usage_with_reset(&s, t(11, 0)),
            "5h 0% · wk 30% · Fable 7% ⏳2d23h"
        );
        // 解除済みの枠は次の枠とまとまらない (resets_at None と同じ扱い)。
        s.seven_day.as_mut().unwrap().resets_at = Some(t(11, 0));
        assert_eq!(
            format_usage_with_reset(&s, t(11, 0)),
            "5h 0% · wk 0% · Fable 7% ⏳2d23h"
        );
    }

    #[test]
    fn with_reset_reads_real_response() {
        // 実応答: 5h は 14:40 (4h40m 後)、wk と Fable は 10/12 18:00 (秒以下だけ違う) で 1 回。
        let s = parse_usage(FIXTURE, t(10, 0)).unwrap();
        assert_eq!(
            format_usage_with_reset(&s, t(10, 0)),
            "5h 1% ⏳4h40m · wk 0% · Fable 0% ⏳6d8h"
        );
    }

    #[test]
    fn status_label_carries_full_and_bare_text() {
        let s = parse_usage(FIXTURE, t(10, 0)).unwrap();
        let label = status_label(&s, t(10, 0)).unwrap();
        assert_eq!(label.full, "5h 1% ⏳4h40m · wk 0% · Fable 0% ⏳6d8h");
        assert_eq!(label.bare, "5h 1% · wk 0% · Fable 0%");
        assert_eq!(label.level, UsageLevel::Normal);
    }

    #[test]
    fn hourglass_is_two_cells_wide() {
        // Windows Terminal は ⏳ を絵文字 (2 桁) で描く。unicode-width の申告も
        // 2 でないと行がはみ出す (ui のレイアウトは unicode-width で測る)。
        assert_eq!(unicode_width::UnicodeWidthStr::width(HOURGLASS), 2);
    }

    // ---- credentials --------------------------------------------------------

    #[test]
    fn credentials_path_prefers_claude_config_dir() {
        let p = credentials_path(
            |k| (k == "CLAUDE_CONFIG_DIR").then(|| "D:\\cc".to_string()),
            Some(PathBuf::from("C:\\Users\\u")),
        );
        assert_eq!(p, Some(PathBuf::from("D:\\cc").join(".credentials.json")));
    }

    #[test]
    fn credentials_path_falls_back_to_home() {
        let home = PathBuf::from("C:\\Users\\u");
        let p = credentials_path(|_| None, Some(home.clone()));
        assert_eq!(p, Some(home.join(".claude").join(".credentials.json")));
        // 空の CLAUDE_CONFIG_DIR は未設定扱い。
        let p = credentials_path(|_| Some("  ".to_string()), Some(home.clone()));
        assert_eq!(p, Some(home.join(".claude").join(".credentials.json")));
        assert_eq!(credentials_path(|_| None, None), None);
    }

    #[test]
    fn access_token_respects_expiry() {
        let json = r#"{"claudeAiOauth":{"accessToken":"tok","expiresAt":1000000}}"#;
        assert_eq!(access_token_from(json, 0), Some("tok".to_string()));
        // 期限の 1 分前を切ったら使わない (更新はしない)。
        assert_eq!(access_token_from(json, 1_000_000 - 60_000), None);
        assert_eq!(access_token_from(json, 2_000_000), None);
    }

    #[test]
    fn access_token_rejects_missing_or_empty() {
        assert_eq!(access_token_from("{}", 0), None);
        assert_eq!(access_token_from("garbage", 0), None);
        assert_eq!(
            access_token_from(r#"{"claudeAiOauth":{"accessToken":"  "}}"#, 0),
            None
        );
        // expiresAt が無ければ期限不明として使う。
        assert_eq!(
            access_token_from(r#"{"claudeAiOauth":{"accessToken":"tok"}}"#, 0),
            Some("tok".to_string())
        );
    }

    // ---- poll config / retry / cache ----------------------------------------

    /// テスト用の env 引き (キーと値の組から引く)。
    fn get(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn poll_config_defaults_and_overrides() {
        assert_eq!(
            poll_config_from(get(&[])),
            PollConfig {
                enabled: true,
                interval: Duration::from_secs(300)
            }
        );
        assert!(!poll_config_from(get(&[("CCNEST_USAGE", "OFF")])).enabled);
        assert!(poll_config_from(get(&[("CCNEST_USAGE", "on")])).enabled);
        assert_eq!(
            poll_config_from(get(&[("CCNEST_USAGE_POLL_SECS", "600")])).interval,
            Duration::from_secs(600)
        );
        // 60 秒未満は 60 秒に切り上げ、不正値は既定。
        assert_eq!(
            poll_config_from(get(&[("CCNEST_USAGE_POLL_SECS", "5")])).interval,
            Duration::from_secs(60)
        );
        assert_eq!(
            poll_config_from(get(&[("CCNEST_USAGE_POLL_SECS", "abc")])).interval,
            Duration::from_secs(300)
        );
    }

    #[test]
    fn retry_waits_depend_on_the_failure() {
        let five_min = Duration::from_secs(300);
        assert_eq!(retry_after(&FetchError::NoToken, five_min), NO_TOKEN_RETRY);
        assert_eq!(
            retry_after(&FetchError::RateLimited, five_min),
            RATE_LIMITED_BACKOFF
        );
        assert_eq!(retry_after(&FetchError::Http(401), five_min), five_min);
        assert_eq!(retry_after(&FetchError::Transport, five_min), five_min);
        assert_eq!(retry_after(&FetchError::BadBody, five_min), five_min);
        // 間隔を 1 時間にしていたら 429 でも 1 時間待つ。
        let hour = Duration::from_secs(3600);
        assert_eq!(retry_after(&FetchError::RateLimited, hour), hour);
    }

    #[test]
    fn cache_is_reused_only_within_the_interval() {
        let s = snap(Some(1.0), None, &[]); // fetched_at = 10:00
        let five_min = Duration::from_secs(300);
        assert_eq!(
            cache_wait(&s, t(10, 2), five_min),
            Some(Duration::from_secs(180))
        );
        assert_eq!(cache_wait(&s, t(10, 5), five_min), None);
        assert_eq!(cache_wait(&s, t(10, 6), five_min), None);
    }

    #[test]
    fn cache_round_trips_through_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ccnest").join("usage-cache.json");
        let s = parse_usage(FIXTURE, t(10, 0)).unwrap();
        write_cache(&path, &s).unwrap();
        assert_eq!(read_cache(&path), Some(s));
        // 書きかけ用の一時ファイルは残らない。
        let left: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, vec![std::ffi::OsString::from("usage-cache.json")]);
    }

    #[test]
    fn read_cache_ignores_broken_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage-cache.json");
        assert_eq!(read_cache(&path), None);
        std::fs::write(&path, "{broken").unwrap();
        assert_eq!(read_cache(&path), None);
    }
}

// Version History
// ver0.1 - 2026-10-06 - Initial: poll the unofficial /api/oauth/usage (the one
//                       /usage uses) on a background thread for the 5-hour,
//                       weekly and per-model weekly (e.g. Fable) limits. Reads
//                       the token from .credentials.json and never refreshes it;
//                       shares results across ccnest windows via
//                       %APPDATA%\ccnest\usage-cache.json. CCNEST_USAGE=off /
//                       CCNEST_USAGE_POLL_SECS.
// ver0.2 - 2026-10-10 - Countdown to each reset: format_remaining (59m / 2h10m /
//                       2d9h, None once passed) and format_usage_with_reset,
//                       which groups adjacent buckets whose resets_at are within
//                       60 s and appends one " ⏳…" per group ("5h 42% ⏳2h10m ·
//                       wk 18% · Fable 7% ⏳2d9h"). status_label returns
//                       UsageLabel { full, bare, level } so the status bar can
//                       fall back to the %-only text when the row is narrow.
