//! Claude ペインの状態検出と、タスク表示文字列 (OSC タイトル由来) の正規化。
//!
//! すべて純関数。I/O・parser ロックは呼び出し側 (`App::refresh_pane_state`,
//! 2 秒 tick) が握り、描画側はキャッシュされた結果を読むだけにする。

/// タブバー/サイドバーに出す Claude の状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClaudeStatus {
    /// 入力待ち (通常色)。
    #[default]
    Idle,
    /// 実行中 (緑)。
    Busy,
    /// 許可/質問プロンプト表示中 (黄・要対応)。
    NeedsAttention,
    /// 非アクティブタブで実行が完了し、まだ閲覧していない (マゼンタ)。
    /// タブを表示すると Idle に戻る。
    DoneUnseen,
}

/// 実行中マーカー: Claude Code がスピナー行に表示する文言。
/// claude v2.1.227 で確認。CLI 更新で変わると検出は Idle に落ちる (安全側)。
pub const BUSY_MARKER: &str = "esc to interrupt";

/// 許可/質問プロンプトのマーカー群。claude v2.1.227 で確認。
/// "Do you want" = ツール実行許可、"Would you like" = プラン承認等、
/// "❯ 1." = 番号付き選択肢 (AskUserQuestion 等)。
pub const ATTENTION_MARKERS: &[&str] = &["Do you want", "Would you like", "❯ 1."];

/// 可視画面テキストから Claude の状態を判定する。
///
/// - `screen_text`: 可視画面の全行テキスト (scrollback は含まない)
/// - `prev`: 前回 tick の判定結果 (Busy→Idle 遷移の検出に使う)
/// - `tab_active`: このペインの属するタブが現在表示中か
///
/// 判定順 (上が勝つ):
/// 1. Busy マーカーあり → Busy。実行中は画面上部に古いプロンプト文言が
///    残っていることがあるため、Busy を Attention より優先する。
/// 2. Attention マーカーあり → NeedsAttention (DoneUnseen の sticky より優先)。
/// 3. それ以外: 非アクティブタブで Busy から抜けたら DoneUnseen (sticky)。
///    アクティブタブなら閲覧済みとして Idle。
pub fn detect_status(screen_text: &str, prev: ClaudeStatus, tab_active: bool) -> ClaudeStatus {
    if screen_text.contains(BUSY_MARKER) {
        return ClaudeStatus::Busy;
    }
    if ATTENTION_MARKERS.iter().any(|m| screen_text.contains(m)) {
        return ClaudeStatus::NeedsAttention;
    }
    match prev {
        ClaudeStatus::Busy | ClaudeStatus::DoneUnseen if !tab_active => ClaudeStatus::DoneUnseen,
        _ => ClaudeStatus::Idle,
    }
}

/// タブ配下の複数ペインの状態を 1 つ (タブの表示色) に畳む。
/// 優先度: NeedsAttention > DoneUnseen > Busy > Idle。
pub fn aggregate_status(statuses: impl IntoIterator<Item = ClaudeStatus>) -> ClaudeStatus {
    fn rank(s: ClaudeStatus) -> u8 {
        match s {
            ClaudeStatus::NeedsAttention => 3,
            ClaudeStatus::DoneUnseen => 2,
            ClaudeStatus::Busy => 1,
            ClaudeStatus::Idle => 0,
        }
    }
    statuses.into_iter().fold(ClaudeStatus::Idle, |acc, s| {
        if rank(s) > rank(acc) {
            s
        } else {
            acc
        }
    })
}

/// タブ先頭に出す状態マーカーの絵文字。文字色より一目で分かる主シグナル。
///
/// Idle も「こちらの指示待ち＝自分の番」なので NeedsAttention と同じ黄色に
/// する (ユーザー要望 2026-08-11)。形は四角セット (同日ユーザー選定)。
/// 🟩🟨🟪 (U+1F7E9/E8/EA) は East Asian Width=Wide で unicode-width=2・
/// Windows Terminal 描画も 2 セルで一致する (✳ U+2733 のような曖昧幅文字と
/// 違い、はみ出しを起こさない)。
pub fn status_marker(status: ClaudeStatus) -> &'static str {
    match status {
        ClaudeStatus::Busy => "🟩",
        ClaudeStatus::NeedsAttention | ClaudeStatus::Idle => "🟨",
        ClaudeStatus::DoneUnseen => "🟪",
    }
}

/// OSC タイトルの生文字列をタスク表示用に正規化する。
///
/// - C0/C1 制御文字を除去
/// - 先頭の装飾グリフ (Claude Code のスピナー ✳ 等・点字スピナー U+2800..=U+28FF)
///   と空白を除去 (先頭のみ。文中のグリフは保持)
/// - 連続空白を 1 個に畳み、前後を trim
/// - 結果が空、または "claude" 単独 (大小無視) なら None (表示価値なし)
pub fn sanitize_task(raw: &str) -> Option<String> {
    // タブ・改行は「空白」として残して後段で 1 個に畳む (is_control は
    // \t \n も真になるため、素朴に落とすと単語が連結されてしまう)。
    let no_ctrl: String = raw
        .chars()
        .filter(|c| !c.is_control() || c.is_whitespace())
        .collect();
    let stripped = no_ctrl.trim_start_matches(|c: char| {
        matches!(
            c,
            '✳' | '✻' | '✽' | '✢' | '·' | '*' | '⏺' | '●' | '○' | '◐' | '◑' | '◒' | '◓'
        ) || ('\u{2800}'..='\u{28FF}').contains(&c)
            || c.is_whitespace()
    });
    let collapsed = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
    // "claude" / "Claude Code" は起動直後の汎用タイトルでタスク情報が無い。
    if collapsed.is_empty()
        || collapsed.eq_ignore_ascii_case("claude")
        || collapsed.eq_ignore_ascii_case("claude code")
    {
        None
    } else {
        Some(collapsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- detect_status ----

    // Busy マーカーがあれば prev/tab_active に関わらず Busy
    #[test]
    fn busy_marker_wins_always() {
        let text = "✳ Deliberating… (esc to interrupt)";
        assert_eq!(
            detect_status(text, ClaudeStatus::Idle, true),
            ClaudeStatus::Busy
        );
        assert_eq!(
            detect_status(text, ClaudeStatus::DoneUnseen, false),
            ClaudeStatus::Busy
        );
    }

    // 実行中に古いプロンプト文言が画面に残っていても Busy が勝つ (誤検知回帰防止)
    #[test]
    fn busy_beats_stale_attention_text() {
        let text = "Do you want to proceed?\n...\n✳ Working (esc to interrupt)";
        assert_eq!(
            detect_status(text, ClaudeStatus::Idle, true),
            ClaudeStatus::Busy
        );
    }

    // 各 Attention マーカー単独で NeedsAttention
    #[test]
    fn do_you_want_is_attention() {
        assert_eq!(
            detect_status("Do you want to make this edit?", ClaudeStatus::Idle, true),
            ClaudeStatus::NeedsAttention
        );
    }

    #[test]
    fn would_you_like_is_attention() {
        assert_eq!(
            detect_status("Would you like to proceed?", ClaudeStatus::Busy, false),
            ClaudeStatus::NeedsAttention
        );
    }

    #[test]
    fn numbered_choice_is_attention() {
        assert_eq!(
            detect_status("❯ 1. Yes\n  2. No", ClaudeStatus::Idle, true),
            ClaudeStatus::NeedsAttention
        );
    }

    // 非アクティブタブで Busy→(マーカー消滅) は DoneUnseen
    #[test]
    fn busy_to_idle_on_background_tab_is_done_unseen() {
        assert_eq!(
            detect_status("❯ ", ClaudeStatus::Busy, false),
            ClaudeStatus::DoneUnseen
        );
    }

    // アクティブタブなら Busy→Idle は素直に Idle (見ているので通知不要)
    #[test]
    fn busy_to_idle_on_active_tab_is_idle() {
        assert_eq!(
            detect_status("❯ ", ClaudeStatus::Busy, true),
            ClaudeStatus::Idle
        );
    }

    // DoneUnseen は非アクティブの間 sticky
    #[test]
    fn done_unseen_sticks_while_background() {
        assert_eq!(
            detect_status("❯ ", ClaudeStatus::DoneUnseen, false),
            ClaudeStatus::DoneUnseen
        );
    }

    // タブを閲覧したら DoneUnseen はクリアされて Idle
    #[test]
    fn done_unseen_clears_on_view() {
        assert_eq!(
            detect_status("❯ ", ClaudeStatus::DoneUnseen, true),
            ClaudeStatus::Idle
        );
    }

    // Busy を経ていない Idle は非アクティブでも Idle のまま
    #[test]
    fn idle_stays_idle_on_background_tab() {
        assert_eq!(
            detect_status("❯ ", ClaudeStatus::Idle, false),
            ClaudeStatus::Idle
        );
    }

    // sticky 中でも Attention マーカーが出たら NeedsAttention が勝つ
    #[test]
    fn attention_beats_done_unseen_sticky() {
        assert_eq!(
            detect_status("Do you want to proceed?", ClaudeStatus::DoneUnseen, false),
            ClaudeStatus::NeedsAttention
        );
    }

    // ---- aggregate_status ----

    // 空イテレータは Idle
    #[test]
    fn aggregate_empty_is_idle() {
        assert_eq!(aggregate_status([]), ClaudeStatus::Idle);
    }

    // Busy > Idle
    #[test]
    fn aggregate_busy_beats_idle() {
        assert_eq!(
            aggregate_status([ClaudeStatus::Idle, ClaudeStatus::Busy]),
            ClaudeStatus::Busy
        );
    }

    // DoneUnseen > Busy
    #[test]
    fn aggregate_done_unseen_beats_busy() {
        assert_eq!(
            aggregate_status([ClaudeStatus::Busy, ClaudeStatus::DoneUnseen]),
            ClaudeStatus::DoneUnseen
        );
    }

    // NeedsAttention > DoneUnseen (順序非依存)
    #[test]
    fn aggregate_attention_beats_all_any_order() {
        assert_eq!(
            aggregate_status([ClaudeStatus::NeedsAttention, ClaudeStatus::DoneUnseen]),
            ClaudeStatus::NeedsAttention
        );
        assert_eq!(
            aggregate_status([
                ClaudeStatus::Idle,
                ClaudeStatus::Busy,
                ClaudeStatus::NeedsAttention
            ]),
            ClaudeStatus::NeedsAttention
        );
    }

    // ---- status_marker ----

    // 実行中は緑四角
    #[test]
    fn marker_busy_is_green() {
        assert_eq!(status_marker(ClaudeStatus::Busy), "🟩");
    }

    // アイドルも「こちらの番」なので黄四角 (許可待ちと同じ)
    #[test]
    fn marker_idle_and_attention_are_yellow() {
        assert_eq!(status_marker(ClaudeStatus::Idle), "🟨");
        assert_eq!(status_marker(ClaudeStatus::NeedsAttention), "🟨");
    }

    // 未閲覧完了は紫四角
    #[test]
    fn marker_done_unseen_is_purple() {
        assert_eq!(status_marker(ClaudeStatus::DoneUnseen), "🟪");
    }

    // ---- sanitize_task ----

    // 装飾なしのタイトルはそのまま通る
    #[test]
    fn sanitize_plain_title_passes() {
        assert_eq!(
            sanitize_task("Fixing the tab bar"),
            Some("Fixing the tab bar".to_string())
        );
    }

    // 先頭のスピナーグリフ + 空白は除去
    #[test]
    fn sanitize_strips_leading_spinner() {
        assert_eq!(
            sanitize_task("✳ Fixing the tab bar"),
            Some("Fixing the tab bar".to_string())
        );
    }

    // 点字スピナー (U+2800 台) も除去
    #[test]
    fn sanitize_strips_braille_spinner() {
        assert_eq!(sanitize_task("⠋ thinking"), Some("thinking".to_string()));
    }

    // 制御文字 (BEL / ESC) は除去
    #[test]
    fn sanitize_strips_control_chars() {
        assert_eq!(sanitize_task("a\x07b\x1bc"), Some("abc".to_string()));
    }

    // 空文字は None
    #[test]
    fn sanitize_empty_is_none() {
        assert_eq!(sanitize_task(""), None);
        assert_eq!(sanitize_task("   "), None);
    }

    // "claude" / "Claude Code" は汎用タイトルなので None (大小無視)
    #[test]
    fn sanitize_bare_claude_is_none() {
        assert_eq!(sanitize_task("Claude"), None);
        assert_eq!(sanitize_task("✳ claude"), None);
        assert_eq!(sanitize_task("Claude Code"), None);
    }

    // 日本語タイトルはそのまま保持される
    #[test]
    fn sanitize_keeps_japanese() {
        assert_eq!(
            sanitize_task("✳ タブ実装中"),
            Some("タブ実装中".to_string())
        );
    }

    // 文中のグリフは除去しない (先頭のみ)
    #[test]
    fn sanitize_keeps_interior_glyphs() {
        assert_eq!(sanitize_task("a ✳ b"), Some("a ✳ b".to_string()));
    }

    // 連続空白は 1 個に畳む
    #[test]
    fn sanitize_collapses_whitespace() {
        assert_eq!(sanitize_task("a   b\tc"), Some("a b c".to_string()));
    }
}

// Version History
// - ver1.2 (2026-08-11): マーカーを四角セット 🟩🟨🟪 に変更 (ユーザー選定)
// - ver1.1 (2026-08-11): status_marker 追加 (タブ先頭の絵文字 🟢🟡🟣。Idle も
//   「こちらの番」として黄色扱い)。汎用タイトル "Claude Code" を task から除外
// - ver1.0 (2026-08-11): 新規作成。ClaudeStatus / detect_status / aggregate_status /
//   sanitize_task (マーカーは claude v2.1.227 で観測)
