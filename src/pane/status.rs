//! Claude ペインの状態検出と、タスク表示文字列 (OSC タイトル由来) の正規化。
//!
//! すべて純関数。I/O・parser ロックは呼び出し側 (`App::refresh_pane_state`,
//! 2 秒 tick) が握り、描画側はキャッシュされた結果を読むだけにする。

/// タブバー/サイドバーに出す Claude の状態。2 値 (実行中か、こちらの番か)。
///
/// 「こちらの番」は質問・許可待ちも作業完了後の入力待ちもまとめて
/// `NeedsAttention` (ユーザー決定 2026-11-08: どちらも自分の対応が要る)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClaudeStatus {
    /// claude が走っていないペイン (shell)。表示なし。
    #[default]
    Idle,
    /// 実行中 (🟢)。
    Busy,
    /// 自分の対応が必要 (🟨): 質問・許可プロンプト、または作業が終わって
    /// 次の指示待ち。実行中でなければすべてこれ。
    NeedsAttention,
}

/// 実行中マーカー (文言): 古い Claude Code がスピナー行に出していた
/// "(esc to interrupt)" (v2.1.227 で確認) と、その別表記。
///
/// **v2.1.319 ではこの文言はスピナー行から消えている** (本体に残る 6 箇所は
/// 再試行待ちと remote 送信の通知だけ・2026-11-08 実測)。そのため文言だけに
/// 頼っていた間、実行中が一度も Busy にならず「実行中なのに 🟨」に見えていた。
/// 現行のスピナー行は [`has_spinner_timer`] で検出し、こちらは旧 CLI 向けに残す。
pub const BUSY_MARKERS: &[&str] = &["esc to interrupt", "enter to interrupt"];

/// 現行 (v2.1.319) のスピナー行 `✢ Metamorphosing… (2m 24s · ↓ 4.4k tokens · …)`
/// を検出する: 省略記号 `…` の後ろ (空白 0 個以上) に `(` と経過時間が続く。
/// 経過時間は `6s` / `2m 24s` / `1h 2m 3s` (60 秒超で分、1 時間超で時が付く。
/// 単位は h → m → s の順・間は空白 1 個・末尾は必ず `s`)、その直後は ` ` か `)`。
///
/// 動詞 (Brewing / Boondoggling …) とスピナー絵文字は毎回変わるので見ない。
/// 完了後の `✻ Worked for 2m 24s · done 10:09` には `…(` が無いので一致しない。
/// 返答本文で `…(12s` / `… (2m 24s` という並びが出ることはまず無い。
///
/// v0.1.31〜33 は `…(` の直結と秒だけ (`<n>s`) を見ていて、`… (2m 24s` のように
/// 空白が入る／分が付く実物を一度も拾えず「実行中なのに 🟨」のままだった。
pub fn has_spinner_timer(screen_text: &str) -> bool {
    const ELLIPSIS: &str = "…";
    let mut rest = screen_text;
    while let Some(i) = rest.find(ELLIPSIS) {
        let after = rest[i + ELLIPSIS.len()..].trim_start_matches(' ');
        if let Some(inner) = after.strip_prefix('(') {
            if elapsed_time_prefix(inner) {
                return true;
            }
        }
        rest = &rest[i + ELLIPSIS.len()..];
    }
    false
}

/// `s` が `<n>h <n>m <n>s` / `<n>m <n>s` / `<n>s` で始まり、その直後が ` ` か `)` なら真。
fn elapsed_time_prefix(s: &str) -> bool {
    // 単位は h → m → s の順にしか現れない。読んだ単位より後ろのものだけ次に許す。
    let mut allowed: &[char] = &['h', 'm', 's'];
    let mut rest = s;
    loop {
        let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 {
            return false;
        }
        let mut tail = rest[digits..].chars();
        let unit = match tail.next() {
            Some(u) if allowed.contains(&u) => u,
            _ => return false,
        };
        let next = tail.next();
        if unit == 's' {
            return matches!(next, Some(' ') | Some(')'));
        }
        if next != Some(' ') {
            return false;
        }
        // "2m " の後ろへ。allowed を m より後ろ (s だけ) / h より後ろ (m, s) に絞る。
        allowed = match unit {
            'h' => &['m', 's'],
            _ => &['s'],
        };
        rest = &rest[digits + unit.len_utf8() + 1..];
    }
}

/// 可視画面テキストから Claude の状態を判定する。
///
/// - `screen_text`: 可視画面の全行テキスト (scrollback は含まない)
///
/// スピナー行 (現行の経過秒つき、または旧 CLI の "esc to interrupt") が
/// あれば Busy、無ければ NeedsAttention。質問文言の検出はしない (Claude の
/// 返答本文に "Would you like" が残っているだけで色が変わる誤検知があり、
/// 2 色化で区別自体が不要になった)。
pub fn detect_status(screen_text: &str) -> ClaudeStatus {
    if has_spinner_timer(screen_text) || BUSY_MARKERS.iter().any(|m| screen_text.contains(m)) {
        ClaudeStatus::Busy
    } else {
        ClaudeStatus::NeedsAttention
    }
}

/// タブ配下の複数ペインの状態を 1 つ (タブのマーカー) に畳む。
/// 優先度: NeedsAttention > Busy > Idle (1 ペインでも対応待ちなら 🟨)。
pub fn aggregate_status(statuses: impl IntoIterator<Item = ClaudeStatus>) -> ClaudeStatus {
    fn rank(s: ClaudeStatus) -> u8 {
        match s {
            ClaudeStatus::NeedsAttention => 2,
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

/// タブ先頭に出す状態マーカーの絵文字。唯一の状態シグナル (タブ名の
/// 文字色は変えない・ユーザー決定 2026-11-08)。
///
/// 実行中は緑の丸、対応待ちはオレンジの四角 (色だけでなく形でも区別)。
/// 🟢 (U+1F7E2) と 🟨 (U+1F7E8) は East Asian Width=Wide で unicode-width=2・
/// Windows Terminal 描画も 2 セルで一致する (✳ U+2733 のような曖昧幅文字と
/// 違い、はみ出しを起こさない)。Idle (shell) は呼び出し側で出さない。
pub fn status_marker(status: ClaudeStatus) -> &'static str {
    match status {
        ClaudeStatus::Busy => "🟢",
        ClaudeStatus::NeedsAttention => "🟨",
        ClaudeStatus::Idle => "",
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

    // 現行 (claude v2.1.319) のスピナー行: 省略記号の直後に (経過秒 ・ …)。
    // 2026-11-08 に使い捨て ccnest の画面から取った実物。
    #[test]
    fn current_spinner_with_timer_is_busy() {
        for line in [
            "✶ Boondoggling…(6s · ↓302 tokens · thought for 1s)",
            "✻ Boondoggling…(7s · ↓455 tokens)",
            "✽Boondoggling…(9s · ↓578 tokens)",
            "· Thinking…(1s)",
            "❯ 指示\n✳ Working…(120s · ↓1.2k tokens)\n──\n❯ ",
        ] {
            assert!(has_spinner_timer(line), "{line}");
            assert_eq!(detect_status(line), ClaudeStatus::Busy, "{line}");
        }
    }

    // 60 秒を超えると経過時間は "2m 24s"、1 時間超は "1h 2m 3s" になり、`…` と `(` の
    // 間に空白が入る (2026-11-09 のゆうさんのスクショ:
    // "✢ Metamorphosing… (2m 24s · ↓ 4.4k tokens · thinking with high effort)")。
    // v0.1.31〜33 はどちらも取りこぼし、実行中が Busy にならず 🟨 のままだった。
    #[test]
    fn spinner_timer_with_minutes_and_space_is_busy() {
        for line in [
            "✢ Metamorphosing… (2m 24s · ↓ 4.4k tokens · thinking with high effort)",
            "✶ Boondoggling… (6s · ↓302 tokens)",
            "✳ Working…(1h 2m 3s · ↓1.2k tokens)",
            "✻ Thinking… (59s)",
            "· Pondering…  (10m 0s)",
        ] {
            assert!(has_spinner_timer(line), "{line}");
            assert_eq!(detect_status(line), ClaudeStatus::Busy, "{line}");
        }
    }

    // 完了行 "Worked for 9s · done 10:09" と、タイマー無しの "Brewing…" は Busy にしない
    #[test]
    fn worked_for_and_bare_ellipsis_are_not_busy() {
        for line in [
            "✻ Worked for 9s · done 10:09 · ↓578 tokens",
            "· Brewing…",
            "読み込み中…(しばらくお待ちください)",
            "…(s)",
            "…(12x)",
            "✻ Worked for 2m 24s · done 10:09",
            "…(2m)",
            "…(2m24s)",
            "…(2m 24)",
            "… 2m 24s",
            "…(2h 3m)",
            "…(3s2m)",
        ] {
            assert!(!has_spinner_timer(line), "{line}");
            assert_eq!(detect_status(line), ClaudeStatus::NeedsAttention, "{line}");
        }
    }

    // 旧 CLI の文言マーカーも引き続き Busy
    #[test]
    fn busy_marker_is_busy() {
        assert_eq!(
            detect_status("✳ Deliberating… (esc to interrupt)"),
            ClaudeStatus::Busy
        );
    }

    // 別表記 "enter to interrupt" も Busy (claude v2.1.319 で併存)
    #[test]
    fn enter_to_interrupt_is_busy() {
        assert_eq!(
            detect_status("✳ Working… (enter to interrupt)"),
            ClaudeStatus::Busy
        );
    }

    // 実行中に古いプロンプト文言が画面に残っていても Busy が勝つ
    #[test]
    fn busy_beats_stale_question_text() {
        let text = "Would you like to proceed?\n...\n✳ Working (esc to interrupt)";
        assert_eq!(detect_status(text), ClaudeStatus::Busy);
    }

    // マーカーが無ければ入力待ちでも質問中でも NeedsAttention (こちらの番)
    #[test]
    fn no_marker_is_attention() {
        assert_eq!(detect_status("❯ "), ClaudeStatus::NeedsAttention);
        assert_eq!(
            detect_status("Do you want to make this edit?"),
            ClaudeStatus::NeedsAttention
        );
        assert_eq!(
            detect_status("❯ 1. Yes\n  2. No"),
            ClaudeStatus::NeedsAttention
        );
        assert_eq!(detect_status(""), ClaudeStatus::NeedsAttention);
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

    // NeedsAttention > Busy (順序非依存)
    #[test]
    fn aggregate_attention_beats_busy_any_order() {
        assert_eq!(
            aggregate_status([ClaudeStatus::Busy, ClaudeStatus::NeedsAttention]),
            ClaudeStatus::NeedsAttention
        );
        assert_eq!(
            aggregate_status([
                ClaudeStatus::NeedsAttention,
                ClaudeStatus::Idle,
                ClaudeStatus::Busy
            ]),
            ClaudeStatus::NeedsAttention
        );
    }

    // ---- status_marker ----

    // 実行中は緑の丸
    #[test]
    fn marker_busy_is_green_circle() {
        assert_eq!(status_marker(ClaudeStatus::Busy), "🟢");
    }

    // 対応待ちはオレンジの四角
    #[test]
    fn marker_attention_is_yellow_square() {
        assert_eq!(status_marker(ClaudeStatus::NeedsAttention), "🟨");
    }

    // shell (Idle) は空 (呼び出し側も出さない)
    #[test]
    fn marker_idle_is_empty() {
        assert_eq!(status_marker(ClaudeStatus::Idle), "");
    }

    // 両マーカーは表示幅 2 (従来の 🟩 と同じ・タブ幅計算の前提)
    #[test]
    fn markers_are_width_two() {
        use unicode_width::UnicodeWidthStr;
        assert_eq!("🟢".width(), 2);
        assert_eq!("🟨".width(), 2);
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
// - ver1.5 (2026-11-09): has_spinner_timer が実物のスピナー行を拾えていなかったのを修正。
//   `…` と `(` の間の空白を許し、経過時間を `<n>h <n>m <n>s` (60 秒超で分・1 時間超で時)
//   まで受ける (elapsed_time_prefix)。v0.1.31〜33 は `…(` 直結かつ秒だけだったので
//   "✢ Metamorphosing… (2m 24s · …)" が一度も Busy にならず 🟨 のままだった
// - ver1.4 (2026-11-09): 対応が必要のマーカーを 🟧 → 🟨 (U+1F7E8) に戻す
//   (ユーザー指示「四角のオレンジは前のオレンジの色に」= v0.1.30 までの 🟨)
// - ver1.3 (2026-11-08): 2 色化 (ユーザー決定)。DoneUnseen と ATTENTION_MARKERS を
//   削除し、detect_status は「スピナー行があるか」だけ (prev / tab_active 不要)。
//   マーカーは 🟢 実行中 (緑の丸) / 🟧 対応が必要 (質問も作業完了も)。
//   has_spinner_timer を追加: claude v2.1.319 のスピナー行 "…(6s · ↓302 tokens)" を
//   検出 (旧文言 "esc to interrupt" はスピナー行から消えていて、実行中が一度も
//   Busy にならなかった＝「実行中なのに 🟨」の原因)。BUSY_MARKERS は旧 CLI 向けに残す
// - ver1.2 (2026-08-11): マーカーを四角セット 🟩🟨🟪 に変更 (ユーザー選定)
// - ver1.1 (2026-08-11): status_marker 追加 (タブ先頭の絵文字 🟢🟡🟣。Idle も
//   「こちらの番」として黄色扱い)。汎用タイトル "Claude Code" を task から除外
// - ver1.0 (2026-08-11): 新規作成。ClaudeStatus / detect_status / aggregate_status /
//   sanitize_task (マーカーは claude v2.1.227 で観測)
