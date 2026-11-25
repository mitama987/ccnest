//! Claude ペインの状態検出と、タスク表示文字列 (OSC タイトル由来) の正規化。
//!
//! すべて純関数。I/O・parser ロックは呼び出し側 (`App::refresh_pane_state`,
//! 2 秒 tick) が握り、描画側はキャッシュされた結果を読むだけにする。

/// タブバー/サイドバーに出す Claude の状態。3 値 (実行中か、返答は終わったが
/// 裏で何かが動いているか、こちらの番か)。
///
/// 「こちらの番」は質問・許可待ちも作業完了後の入力待ちもまとめて
/// `NeedsAttention` (ユーザー決定 2026-11-08: どちらも自分の対応が要る)。
/// `Background` は 2026-11-14 に追加。当初は別マーカー 🔵 だったが、2026-11-15 の
/// ユーザー決定で表示は `Busy` と同じ 🟢 ([`status_marker`])。判定は分けたまま。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClaudeStatus {
    /// claude が走っていないペイン (shell)。表示なし。
    #[default]
    Idle,
    /// 実行中 (🟢)。
    Busy,
    /// 裏で実行中 (表示は実行中と同じ 🟢): 返答は終わっているが、裏のシェル・エージェントなどが
    /// 動いている (終われば Claude に通知が届き、自分で続きを始める)。
    Background,
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

/// フッターの「裏で動いているもの」の札に出る名詞 (Claude Code v2.1.325 の本体で
/// 確認・2026-11-14)。札は `<n> <名詞>` で、種類が混ざると `<n> background tasks`、
/// シェルと monitor だけは `1 shell, 1 monitor` と並ぶ。
///
/// わざと入れていないもの:
/// - `Artifact comment monitor`: アーティファクトを公開したあとずっと出る見張り
/// - `dreaming` / `saving memories` / `memory import` / `auto-mode scan`: 本体の後片付け
/// - `← 8 agents` の `agents`: 札ではなくエージェント一覧への案内
const BACKGROUND_NOUNS: &[&str] = &[
    "shell",
    "shells",
    "monitor",
    "monitors",
    "local agent",
    "local agents",
    "team",
    "teams",
    "background task",
    "background tasks",
    "background dynamic workflow",
    "background dynamic workflows",
    "remote dynamic workflow",
    "remote dynamic workflows",
    "cloud session",
    "cloud sessions",
    "MCP task",
    "MCP tasks",
];

/// 返答が終わったあとも裏で何かが動いているか。入力欄の下のフッター
/// `  ⏵⏵ bypass permissions on · 1 shell · ← 8 agents          /rc` の札を見る。
///
/// 見るのは「画面で最後の罫線 (`─` が 3 個以上並んだ所) より後ろ」だけ。
/// 入力欄の下枠がその罫線なので、後ろはフッターになる。罫線が無い画面
/// (許可ダイアログなど入力欄が出ていない場面) は false。
/// 行ではなく罫線の終わりで切るのは、vt100 の `contents()` が幅いっぱいの行を
/// 次の行と改行なしでつなぐことがあるため (実測: `────…────❯ `)。
///
/// 完了行 `✻ Sautéed for 3s · done 18:08 · 1 shell still running` は使わない。
/// 会話に残る文字で、裏の仕事が終わったあとも画面に残りうる (罫線より上にあり、
/// 札の文言とも完全一致しないので当たらない)。
pub fn has_background_work(screen_text: &str) -> bool {
    let Some((_, end)) = last_rule(screen_text) else {
        return false;
    };
    screen_text[end..]
        .lines()
        .flat_map(|line| line.split('·'))
        .flat_map(|piece| piece.split("  "))
        .any(|token| is_background_pill(token.trim()))
}

/// 画面で最後の罫線 (`─` が 3 個以上並んだ所) の (開始, 終了) バイト位置。
/// Claude Code の画面では入力欄の下枠。1〜2 個だけの `─` は罫線ではない
/// (本文やエージェント一覧に混ざった記号) とみなして読み飛ばす。
///
/// 入力欄の上枠は右端にセッション名の札が付いて `──── name ─` で終わることが
/// あるが、長い並びのほうを取るので札の後ろの `─` 1 個には引っかからない。
fn last_rule(text: &str) -> Option<(usize, usize)> {
    const RULE: char = '─';
    let mut found = None;
    let mut run: Option<(usize, usize)> = None; // (開始位置, 個数)
    for (i, c) in text.char_indices() {
        if c != RULE {
            run = None;
            continue;
        }
        let (start, n) = run.map_or((i, 1), |(start, n)| (start, n + 1));
        run = Some((start, n));
        if n >= 3 {
            found = Some((start, i + RULE.len_utf8()));
        }
    }
    found
}

/// `1 shell` / `2 local agents` / `1 shell, 1 monitor` / `◇ 1 cloud session` の形か。
/// 区切った 1 つ分と完全に一致するときだけ真 (`1 shell still running` は偽)。
fn is_background_pill(token: &str) -> bool {
    // クラウド系の札は先頭に記号 1 つと空白が付く。
    let body = strip_leading_glyph(token);
    !body.is_empty()
        && body
            .split(", ")
            .all(|part| is_count_and_noun(part, BACKGROUND_NOUNS))
}

/// 先頭が「英数字でない 1 文字 + 空白」なら、それを外す (`◇ 1 cloud session` /
/// `✻ Waiting for …`)。
fn strip_leading_glyph(s: &str) -> &str {
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(glyph), Some(' ')) if !glyph.is_alphanumeric() => chars.as_str(),
        _ => s,
    }
}

/// `<数字> <nouns のどれか>` か。
fn is_count_and_noun(part: &str, nouns: &[&str]) -> bool {
    let digits = part.chars().take_while(|c| c.is_ascii_digit()).count();
    digits > 0
        && part[digits..]
            .strip_prefix(' ')
            .is_some_and(|noun| nouns.contains(&noun))
}

/// 待ちの行 `Waiting for <n> <名詞> [and <n> <名詞>] to finish` に出る名詞
/// (Claude Code v2.1.325 の本体で確認・2026-11-15)。
const WAITING_NOUNS: &[&str] = &[
    "background agent",
    "background agents",
    "dynamic workflow",
    "dynamic workflows",
];

/// 返答は終わったが、裏のエージェント (または dynamic workflow) の終わりを待って
/// いるか。入力欄のすぐ上の行 `✻ Waiting for 1 background agent to finish` を見る。
///
/// この場面はフッターに札 (`1 local agent`) が出ず、[`has_background_work`] では
/// 拾えない (裏のエージェントはフッターの下の一覧 `◯ general-purpose …` に出る・
/// 2026-11-15 のゆうさんのスクショと CCNEST_STATUS_TRACE の実物)。
///
/// 待ちの行はターン終了の行 (`✻ Worked for 9s · done 10:09` と同じ場所) の
/// 差し替えで、会話に残る文字。エージェントが終わると通知で次のターンが始まり、
/// 行の下に新しい内容が足されるので、「入力欄の上枠より前の、最後の区切りの行」が
/// この文言のときだけ真にする (古い行には当たらない)。
/// 入力欄 (上枠と下枠の罫線 2 本) が無い画面は false。
///
/// 「区切りの行」は 0 桁目から始まる行 ([`starts_at_column_zero`])。Claude Code は
/// ターンの区切り (`●` / `✻` / `❯`) を必ず 0 桁目から書き、子の行 (`  ⎿ …`) と
/// 折り返しの続きは字下げする。待ちの行の下に足されるのが字下げの行だけなら、
/// まだ待っている。
/// 放置中のまとめ (idle compaction) は、ターンを始めずに待ちの行の下へ子の行
/// (`  ⎿  Referenced file …`) とお知らせ (`● Compacted while idle, …`) を足す
/// (2026-11-18 のゆうさんのスクショ: workflow が動いているのに 🟨 だった)。
/// 最後の区切りの行がこのお知らせなら、その手前の区切りの行を見る。
pub fn is_waiting_for_background(screen_text: &str) -> bool {
    transcript(screen_text).is_some_and(|t| {
        t.lines()
            .rev()
            .filter(|line| starts_at_column_zero(line))
            .find(|line| !is_idle_compaction_notice(line))
            .is_some_and(|line| is_waiting_line(line.trim_end()))
    })
}

/// 行が 0 桁目から始まるか (空行・字下げされた行は偽)。
fn starts_at_column_zero(line: &str) -> bool {
    line.chars().next().is_some_and(|c| !c.is_whitespace())
}

/// 放置中のまとめ (idle compaction) のお知らせの出だし (Claude Code v2.1.327 の本体で
/// 確認・2026-11-18)。後ろに ` · turn off with "idleCompaction": false in settings.json`
/// が続き、狭いペインでは折り返す (続きは字下げ) ので、出だしだけを見る。
/// 手で打つ `/compact` の `Conversation compacted` はターンの区切りなので入れない。
const IDLE_COMPACTION_NOTICES: &[&str] = &[
    "Compacted while idle, before the prompt cache expired",
    "Compacted using a summary written while you were away",
];

/// `● Compacted while idle, before the prompt cache expired · turn off with …` の形か。
fn is_idle_compaction_notice(line: &str) -> bool {
    let body = strip_leading_glyph(line);
    IDLE_COMPACTION_NOTICES
        .iter()
        .any(|notice| body.starts_with(notice))
}

/// 画面のうち会話の部分 (入力欄の上枠の行より前)。入力欄 (上枠と下枠の罫線
/// 2 本) が無い画面は None。行の途中では切らない。
fn transcript(screen_text: &str) -> Option<&str> {
    let (bottom, _) = last_rule(screen_text)?;
    let above_bottom = &screen_text[..bottom];
    let (top, _) = last_rule(above_bottom)?;
    let end = above_bottom[..top].rfind('\n').map_or(0, |i| i + 1);
    Some(&above_bottom[..end])
}

/// 返答は終わったが、裏のエージェントがまだ動いているか。フッターの下の一覧
/// (`● main` の下) に出る、動いているエージェントの行を見る:
/// `  ◯ general-purpose  Run ping command        2m 17s · ↓ 47.7k tokens`
///
/// 待ちの行 ([`is_waiting_for_background`]) は出ないターンがある (2026-11-24 の
/// ゆうさんのスクショ: 裏のエージェントが動いているのに入力欄の上に待ちの行が無く、
/// フッターに札も無いので 🟨 だった。本体 v2.1.331 では、待ちの行はターン終了の行の
/// 差し替えで、ターン終了の行そのものを作らない経路がある)。一覧の行は、動いている
/// 間は必ず出て、エージェントが終わると一覧ごと消える (v2.1.331 の実物で確認)。
///
/// 見るのは [`has_background_work`] と同じ「最後の罫線より後ろ」だけ。会話の本文に
/// 同じ形の行があっても当たらない。入力欄が無い画面は false。
pub fn has_running_agent_row(screen_text: &str) -> bool {
    let Some((_, end)) = last_rule(screen_text) else {
        return false;
    };
    screen_text[end..].lines().any(is_running_agent_row)
}

/// 一覧の行のうち、動いているエージェントの行か:
/// `<経過時間> · ↓ <数> tokens` で終わる (経過時間は `6s` / `2m 24s` / `1h 2m 3s` で、
/// 必ず秒で終わる)。
///
/// 当たらない行: `● main`・案内 (`← 8 agents` / `↓ to manage` /
/// `/tasks to see subagents`)・利用制限のお知らせ・出だしの 1 秒の行 (`… 0s` だけで
/// トークン数がまだ無い。この瞬間は返答中なので拾わなくてよい)。
/// 行頭の丸 (`◯` / `○`) とエージェントの種類・説明は見ない (毎回変わる)。
fn is_running_agent_row(line: &str) -> bool {
    // 後ろから読む: ` tokens` ← `<数>` ← `↓` ← `·` ← `<n>s` (経過時間の最後の語)。
    let Some(head) = line.trim_end().strip_suffix(" tokens") else {
        return false;
    };
    let Some((head, count)) = head.rsplit_once('↓') else {
        return false;
    };
    if !is_token_count(count.trim()) {
        return false;
    }
    let Some(head) = head.trim_end().strip_suffix('·') else {
        return false;
    };
    head.split_whitespace()
        .next_back()
        .is_some_and(is_seconds_word)
}

/// トークン数の表記か: `302` / `47.7k` / `116.0k` / `1.2m` (数字と小数点 + 単位 1 文字まで)。
fn is_token_count(s: &str) -> bool {
    let digits = s.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    s.len() - digits.len() <= 1
        && digits.starts_with(|c: char| c.is_ascii_digit())
        && digits.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// 経過時間の最後の語 `<n>s` か (`6s` / `2m 24s` の `24s`)。
fn is_seconds_word(word: &str) -> bool {
    word.strip_suffix('s')
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// 「判断待ち」の見出しの末尾に来る言葉。返答の最後に
/// `▼ ゆうさんの判断待ち` の節を置き、無ければ `・今はなし` と書く報告の型を前提にする。
const DECISION_HEADINGS: &[&str] = &["判断待ち"];
/// 見出しとみなす行の長さの上限 (文字数)。これより長い行は文章。
const DECISION_HEADING_MAX_CHARS: usize = 30;
/// 「判断待ちは無い」と読む項目の出だし。
const DECISION_NONE_WORDS: &[&str] = &["なし", "無し", "ありません", "none", "None", "NONE"];
/// 「なし」の前に付く言葉 (`今はなし`・`特になし`)。
const DECISION_NONE_LEADS: &[&str] = &[
    "今のところ",
    "今は",
    "現時点では",
    "現在は",
    "特に",
    "とくに",
];
/// 箇条書きの行頭に来る記号。
const BULLETS: &[char] = &['・', '･', '-', '•', '*', '–', '—'];

/// 返答が「ユーザーの判断待ち」で終わっているか。裏で何かが動いていても、
/// これが真ならこちらの番 (🟨) にする (ユーザー決定 2026-11-15)。
///
/// 見るのは会話の部分 ([`transcript`]) にある最後の「判断待ち」の見出し:
/// - 見出しのすぐ下の行 (最初の項目) が「なし」の類なら偽 (`・今はなし`)
/// - 見出しのすぐ下が空行 (中身が無い)・見出しが画面に無いときも偽
/// - 見出しより後ろにユーザーの発話 (`❯ …`) があれば偽 (もう答えている)
///
/// 質問文言 ("Would you like …") を探す方式は誤検知が多くてやめた経緯がある。
/// ここは文言ではなく報告の型 (見出し + 項目) を見るので、本文に「判断待ち」と
/// 書いてあるだけでは当たらない。型を使わない返答には効かない (今までどおり)。
pub fn asks_for_decision(screen_text: &str) -> bool {
    let Some(transcript) = transcript(screen_text) else {
        return false;
    };
    let lines: Vec<&str> = transcript.lines().map(str::trim).collect();
    let Some(heading) = lines.iter().rposition(|line| is_decision_heading(line)) else {
        return false;
    };
    let after = &lines[heading + 1..];
    if after.iter().any(|line| line.starts_with('❯')) {
        return false;
    }
    after
        .first()
        .is_some_and(|item| !item.is_empty() && !is_none_item(item))
}

/// `▼ ゆうさんの判断待ち` / `■ 5. ゆうさんの判断待ち` / `判断待ち：` の形か。
/// 短い行が「判断待ち」(+ コロン) で終わるときだけ真。箇条書きの行は見出しではない。
fn is_decision_heading(line: &str) -> bool {
    if line.starts_with(BULLETS) || line.chars().count() > DECISION_HEADING_MAX_CHARS {
        return false;
    }
    let body = line.trim_end_matches([':', '：', ' ', '　']);
    DECISION_HEADINGS.iter().any(|word| body.ends_with(word))
}

/// `・今はなし` / `なし` / `- 特になし` / `・None` のように、中身が無いと読む項目か。
fn is_none_item(item: &str) -> bool {
    let mut body = item.trim_start_matches(|c: char| BULLETS.contains(&c) || c.is_whitespace());
    for lead in DECISION_NONE_LEADS {
        if let Some(rest) = body.strip_prefix(lead) {
            body = rest.trim_start();
            break;
        }
    }
    DECISION_NONE_WORDS
        .iter()
        .any(|word| body.starts_with(word))
}

/// `[記号 ]Waiting for 1 background agent to finish` /
/// `… 2 background agents and 1 dynamic workflow to finish` の形か (完全一致)。
fn is_waiting_line(line: &str) -> bool {
    strip_leading_glyph(line)
        .strip_prefix("Waiting for ")
        .and_then(|rest| rest.strip_suffix(" to finish"))
        .is_some_and(|counts| {
            counts
                .split(" and ")
                .all(|part| is_count_and_noun(part, WAITING_NOUNS))
        })
}

/// 画面の各行 (vt100 の `Screen::rows`) を、判定用の 1 つの文字列にする。
/// 行は必ず改行で区切り、行末の空白と末尾の空行を落とす。
///
/// vt100 の `Screen::contents()` は使わない: 最終列まで埋まった行 (wrapped) を
/// 次の行と改行なしでつなぐので、Claude Code が行を空白で最終列まで埋めて書いた
/// ときに、別々の行 (`▼ ゆうさんの判断待ち` と `・項目`) が 1 行になる
/// (2026-11-15 に実物で確認。v0.1.41 までは contents() を渡していた)。
pub fn screen_text(rows: impl IntoIterator<Item = String>) -> String {
    let mut text = String::new();
    for row in rows {
        text.push_str(row.trim_end());
        text.push('\n');
    }
    text.truncate(text.trim_end_matches('\n').len());
    text
}

/// 可視画面テキストから Claude の状態を判定する。
///
/// - `screen_text`: 可視画面の全行テキスト ([`screen_text`] で作る。scrollback は含まない)
///
/// スピナー行 (現行の経過秒つき、または旧 CLI の "esc to interrupt") が
/// あれば Busy。無くて、フッターに裏の仕事の札がある ([`has_background_work`])
/// か、入力欄のすぐ上が裏のエージェント待ちの行 ([`is_waiting_for_background`])
/// か、フッターの下の一覧に動いているエージェントの行がある
/// ([`has_running_agent_row`]) なら Background。どれも無ければ NeedsAttention。
/// ただし裏の仕事があっても、返答が「判断待ち」の節 (中身あり) で終わって
/// いれば NeedsAttention ([`asks_for_decision`])。
/// 質問文言の検出はしない (Claude の返答本文に "Would you like" が残っている
/// だけで色が変わる誤検知があり、区別自体をやめた)。そのため、裏の仕事が
/// 動いている間に Claude が「判断待ち」の節を使わず文章だけで質問して返答を
/// 終えた場合は Background のまま。
pub fn detect_status(screen_text: &str) -> ClaudeStatus {
    if has_spinner_timer(screen_text) || BUSY_MARKERS.iter().any(|m| screen_text.contains(m)) {
        ClaudeStatus::Busy
    } else if (has_background_work(screen_text)
        || is_waiting_for_background(screen_text)
        || has_running_agent_row(screen_text))
        && !asks_for_decision(screen_text)
    {
        ClaudeStatus::Background
    } else {
        ClaudeStatus::NeedsAttention
    }
}

/// Claude Code の入力欄の先頭に出る記号 (ユーザーの発話の行頭も同じ記号)。
const PROMPT: &str = "❯";

/// 入力欄が「空で」出ているか。上枠と下枠の罫線 2 本のあいだが、プロンプト記号
/// `❯` だけの 1 行 (後ろは空白・NBSP だけ) のときに真。
///
/// 偽になる画面:
/// - 入力欄が無い (許可・質問・計画承認のダイアログは入力欄を置き換える)
/// - 入力欄に書きかけの文字がある (1 行でも複数行でも)
///
/// 上枠は右端にセッション名の札が付いて `──── name ─` で終わることがあるので、
/// 上枠の行の残りは読み飛ばす。
pub fn at_empty_prompt(screen_text: &str) -> bool {
    let Some((bottom, _)) = last_rule(screen_text) else {
        return false;
    };
    let above = &screen_text[..bottom];
    let Some((_, top_end)) = last_rule(above) else {
        return false;
    };
    let after_top = &above[top_end..];
    let Some(line_end) = after_top.find('\n') else {
        return false;
    };
    let mut lines = after_top[line_end + 1..]
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    lines.next() == Some(PROMPT) && lines.next().is_none()
}

/// 止めても失う物が無い画面か (保留にしたタブの Claude を落としてメモリを空けて
/// よいか)。次の全部を満たすときだけ真:
/// - 実行中でない (スピナー行・旧 CLI の "esc to interrupt" が無い)
/// - 裏の仕事が無い (フッターの札 `1 shell` など・`Waiting for … to finish`・
///   フッターの下の一覧の動いているエージェントの行)。
///   [`detect_status`] は「判断待ち」の節があれば裏の仕事があっても
///   NeedsAttention を返すが、ここでは裏の仕事があれば必ず偽 (落とすと止まる)
/// - 入力欄が空で出ている ([`at_empty_prompt`])。ダイアログの途中や、書きかけの
///   入力がある画面は偽 (落とすとダイアログ・書きかけが消える)
///
/// 「真だと言えるときだけ落とす」向き: 本体の版が変わって画面の形が読めなく
/// なったら偽になり、落とさない側に倒れる。
pub fn is_safe_to_stop(screen_text: &str) -> bool {
    !has_spinner_timer(screen_text)
        && !BUSY_MARKERS.iter().any(|m| screen_text.contains(m))
        && !has_background_work(screen_text)
        && !is_waiting_for_background(screen_text)
        && !has_running_agent_row(screen_text)
        && at_empty_prompt(screen_text)
}

/// 「返答は終わったが、まだ見ていない」の目印を次の tick でどうするか。
///
/// 裏でシェル・エージェントが動いていると判定は [`ClaudeStatus::Background`]
/// (🟢) になるが、返答 (報告) 自体は終わっていてユーザーが読む番のことが多い
/// (2026-11-23 のゆうさんのスクショ: `1 shell` が残った報告のタブが 🟢)。
/// そこで返答が終わった瞬間 (Busy → Background) にそのタブを見ていなければ
/// 目印を立て、立っている間は 🟨 で出す ([`displayed_status`])。
/// 一度そのタブを開いたら下ろし、裏が動いている間は 🟢 に戻す (ユーザー決定
/// 2026-11-23「見るまでオレンジ」)。
///
/// - `prev`: 前回の tick の判定 (目印で 🟨 にする前の値)
/// - `now`: 今回の判定
/// - `unseen`: 前回までの目印
/// - `viewing`: このペインが今見ているタブにあるか
///
/// 判定が Background 以外になったら下ろす (Busy = 続きを始めた・
/// NeedsAttention = もともと 🟨)。前回の状態が Busy でない Background
/// (窓を開いた直後・再開したタブ) では立てない。
pub fn unseen_turn_end(prev: ClaudeStatus, now: ClaudeStatus, unseen: bool, viewing: bool) -> bool {
    !viewing && now == ClaudeStatus::Background && (unseen || prev == ClaudeStatus::Busy)
}

/// タブ・サイドバーに出す状態。「返答は終わったが、まだ見ていない」の目印
/// ([`unseen_turn_end`]) が立っていれば対応待ち (🟨)、無ければ判定どおり。
pub fn displayed_status(detected: ClaudeStatus, unseen: bool) -> ClaudeStatus {
    if unseen {
        ClaudeStatus::NeedsAttention
    } else {
        detected
    }
}

/// タブ配下の複数ペインの状態を 1 つ (タブのマーカー) に畳む。
/// 優先度: NeedsAttention > Busy > Background > Idle (1 ペインでも対応待ちなら 🟨)。
pub fn aggregate_status(statuses: impl IntoIterator<Item = ClaudeStatus>) -> ClaudeStatus {
    fn rank(s: ClaudeStatus) -> u8 {
        match s {
            ClaudeStatus::NeedsAttention => 3,
            ClaudeStatus::Busy => 2,
            ClaudeStatus::Background => 1,
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
/// 実行中と裏で実行中は緑の丸、対応待ちはオレンジの四角 (自分の番だけ形が
/// 違う)。裏で実行中は v0.1.36〜39 では青の丸 🔵 だったが、同じ 🟢 にそろえた
/// (ユーザー決定 2026-11-15「青の判定時も緑アイコンに」)。判定 ([`ClaudeStatus::
/// Background`]) は分けたまま残してあるので、また分けるときはここの絵文字だけ変える。
/// 🟢 (U+1F7E2)・🟨 (U+1F7E8) は East Asian Width=Wide で unicode-width=2・
/// Windows Terminal 描画も 2 セルで一致する (✳ U+2733 のような曖昧幅文字と
/// 違い、はみ出しを起こさない)。Idle (shell) は呼び出し側で出さない。
pub fn status_marker(status: ClaudeStatus) -> &'static str {
    match status {
        ClaudeStatus::Busy | ClaudeStatus::Background => "🟢",
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

    // ---- unseen_turn_end / displayed_status ----

    use ClaudeStatus::{Background, Busy, Idle, NeedsAttention};

    // 見ていないタブで返答が終わり、裏の仕事が残った → 見るまで 🟨。
    #[test]
    fn turn_end_with_background_while_away_is_unseen() {
        assert!(unseen_turn_end(Busy, Background, false, false));
        assert_eq!(displayed_status(Background, true), NeedsAttention);
    }

    // そのタブを見ている間に返答が終わったなら、もう見た扱い (🟢 のまま)。
    #[test]
    fn turn_end_while_viewing_is_seen() {
        assert!(!unseen_turn_end(Busy, Background, false, true));
        assert_eq!(displayed_status(Background, false), Background);
    }

    // 一度立ったら、開くまで裏の仕事が続くかぎり立ったまま。開いたら下りる。
    #[test]
    fn unseen_stays_until_viewed() {
        assert!(unseen_turn_end(Background, Background, true, false));
        assert!(!unseen_turn_end(Background, Background, true, true));
        // 開いたあとに別のタブへ移っても、もう 🟨 には戻らない。
        assert!(!unseen_turn_end(Background, Background, false, false));
    }

    // 裏の仕事が終わって Claude が続きを始めた／対応待ちになったら下ろす
    // (Busy は 🟢、NeedsAttention はもともと 🟨)。
    #[test]
    fn unseen_clears_when_status_moves_on() {
        assert!(!unseen_turn_end(Background, Busy, true, false));
        assert!(!unseen_turn_end(Background, NeedsAttention, true, false));
        assert!(!unseen_turn_end(Background, Idle, true, false));
    }

    // 前回の状態が分からない (窓を開いた直後・再開したタブ) ときは立てない
    // (今までどおり 🟢)。前回が対応待ちから裏の仕事に変わることは普通は無い。
    #[test]
    fn background_without_turn_end_is_not_unseen() {
        assert!(!unseen_turn_end(Idle, Background, false, false));
        assert!(!unseen_turn_end(NeedsAttention, Background, false, false));
    }

    // 立っていなければ判定どおりに出す。
    #[test]
    fn displayed_status_passes_through_when_seen() {
        for s in [Idle, Busy, Background, NeedsAttention] {
            assert_eq!(displayed_status(s, false), s);
        }
    }

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

    // ---- at_empty_prompt / is_safe_to_stop ----

    /// 入力欄の中身 (`box_body`) を差し替えた画面。
    fn screen_with_box(body: &str, box_body: &str, footer: &str) -> String {
        let rule = "─".repeat(72);
        format!("{body}\n{rule}\n{box_body}\n{rule}\n  {footer}\n")
    }

    const IDLE_FOOTER: &str = "⏸ plan mode on (shift+tab to cycle) · ← 8 agents";

    // 空の入力欄: 記号だけ・後ろに空白・後ろに NBSP (どれも実物にある形)
    #[test]
    fn empty_prompt_is_detected_in_its_real_shapes() {
        for prompt in ["❯", "❯ ", "❯\u{a0}"] {
            let text = screen_with_box("● 終わったよ。", prompt, IDLE_FOOTER);
            assert!(at_empty_prompt(&text), "{prompt:?}");
            assert!(is_safe_to_stop(&text), "{prompt:?}");
        }
        // 上枠の右端にセッション名の札が付く形
        let rule = "─".repeat(60);
        let text =
            format!("● 終わったよ。\n{rule} hold-tab ─\n❯ \n{rule}────────────\n  {IDLE_FOOTER}\n");
        assert!(at_empty_prompt(&text));
        assert!(is_safe_to_stop(&text));
    }

    // 書きかけの入力がある画面は落とさない (落とすと書きかけが消える)
    #[test]
    fn a_draft_in_the_input_box_is_not_safe_to_stop() {
        let one = screen_with_box("● 終わったよ。", "❯ 途中まで書いた", IDLE_FOOTER);
        assert!(!at_empty_prompt(&one));
        assert!(!is_safe_to_stop(&one));
        let two = screen_with_box("● 終わったよ。", "❯ 1 行目\n  2 行目", IDLE_FOOTER);
        assert!(!at_empty_prompt(&two));
        let blank_then_text = screen_with_box("● 終わったよ。", "❯\n  2 行目", IDLE_FOOTER);
        assert!(!at_empty_prompt(&blank_then_text));
    }

    // 入力欄が無い画面 (許可・質問のダイアログ) は落とさない
    #[test]
    fn a_dialog_is_not_safe_to_stop() {
        let dialog = "● Bash(rm -rf build)\n\nDo you want to proceed?\n❯ 1. Yes\n  2. No";
        assert_eq!(detect_status(dialog), ClaudeStatus::NeedsAttention);
        assert!(!at_empty_prompt(dialog));
        assert!(!is_safe_to_stop(dialog));
        // 罫線が 1 本だけ (ダイアログの上の線) でも偽
        let rule = "─".repeat(72);
        let one_rule = format!("● plan\n{rule}\n Ready to code?\n❯ 1. Yes\n  2. No");
        assert!(!at_empty_prompt(&one_rule));
        assert!(!at_empty_prompt(""));
        assert!(!at_empty_prompt("❯ "));
    }

    // 実行中・裏の仕事があるときは、入力欄が空でも落とさない
    #[test]
    fn working_or_background_is_not_safe_to_stop() {
        let busy = screen_with_box(
            "❯ 指示\n\n✶ Boondoggling… (6s · ↓302 tokens)",
            "❯ ",
            "⏵⏵ bypass permissions on · esc to interrupt",
        );
        assert!(at_empty_prompt(&busy));
        assert!(!is_safe_to_stop(&busy));
        // 実物: 裏でシェルが動いている (フッターの札 `1 shell`)
        assert!(at_empty_prompt(REAL_BACKGROUND_SCREEN));
        assert!(!is_safe_to_stop(REAL_BACKGROUND_SCREEN));
        // 裏のエージェント待ち
        let waiting = screen_with_box(
            "● started\n\n✻ Waiting for 1 background agent to finish\n",
            "❯ ",
            "⏵⏵ bypass permissions on · ← 8 agents",
        );
        assert!(!is_safe_to_stop(&waiting));
    }

    // 「判断待ち」の節があると detect_status は 🟨 (NeedsAttention) を返すが、
    // 裏の仕事が動いているなら落とさない (落とすと裏の仕事が止まる)
    #[test]
    fn decision_pending_with_background_work_is_not_safe_to_stop() {
        let text = screen_with_box(
            "▼ ゆうさんの判断待ち\n・マーク: A か B のどれにするか\n\n✻ Churned for 5s · done 17:06 · 1 shell still running\n",
            "❯ ",
            "⏵⏵ bypass permissions on · 1 shell",
        );
        assert_eq!(detect_status(&text), ClaudeStatus::NeedsAttention);
        assert!(!is_safe_to_stop(&text));
        // 同じ画面で裏の仕事が終わっていれば落としてよい
        let done = screen_with_box(
            "▼ ゆうさんの判断待ち\n・マーク: A か B のどれにするか\n\n✻ Churned for 5s · done 17:06\n",
            "❯ ",
            "⏵⏵ bypass permissions on",
        );
        assert!(is_safe_to_stop(&done));
    }

    // ---- has_background_work / Background ----

    /// Claude Code の入力欄まわり (上枠・プロンプト・下枠・フッター) を付けた画面。
    fn screen_with_footer(body: &str, footer: &str) -> String {
        let rule = "─".repeat(72);
        format!("{body}\n{rule}\n❯ \n{rule}\n  {footer}\n")
    }

    // 2026-11-14 に使い捨て ccnest の CCNEST_STATUS_TRACE で取った実物
    // (claude v2.1.325・裏で Start-Sleep 60 を走らせて返答を終えた直後)。
    const REAL_BACKGROUND_SCREEN: &str = "\
  Then reply with only the word started.

● PowerShell(Start-Sleep -Seconds 60)
  ⎿  Running in the background (↓ to manage)

● started

✻ Sautéed for 3s · done 18:08 · 1 shell still running

────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────
  ⏵⏵ bypass permissions on · 1 shell · ← 8 agents                  /rc
";

    // 同じ実行で、裏のシェルが終わって通知の返答も終わったあとの実物。上枠と
    // プロンプトが改行なしでつながっている (vt100 の contents() の折り返し扱い)。
    const REAL_DONE_SCREEN: &str = "\
● Background command \"Sleep for 60 seconds in the background\" completed
(exit code 0)

● ゆうさん、バックグラウンドの sleep 60 が終了コード 0 で完了したよ✨

✻ Cooked for 1s · done 18:09

────────────────────────────────────────────────────────────────────────❯
────────────────────────────────────────────────────────────────────────
  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← 8 agents       /rc
";

    // 返答は終わり、フッターに "1 shell" の札 → 裏で実行中
    #[test]
    fn real_screen_with_background_shell_is_background() {
        assert!(has_background_work(REAL_BACKGROUND_SCREEN));
        assert_eq!(
            detect_status(REAL_BACKGROUND_SCREEN),
            ClaudeStatus::Background
        );
    }

    // 裏のシェルが終わってフッターの札が消えたら、こちらの番に戻る
    #[test]
    fn real_screen_after_background_finished_is_attention() {
        assert!(!has_background_work(REAL_DONE_SCREEN));
        assert_eq!(
            detect_status(REAL_DONE_SCREEN),
            ClaudeStatus::NeedsAttention
        );
    }

    // 札の文言 (本体の関数が作る形) はどれも裏で実行中
    #[test]
    fn footer_pills_are_background() {
        for footer in [
            "⏵⏵ bypass permissions on · 1 shell · ← 8 agents                  /rc",
            "⏵⏵ bypass permissions on · 2 shells · ⧉ index · ← 8 agents",
            "⏵⏵ bypass permissions on · 1 local agent · ← 8 agents",
            "⏵⏵ bypass permissions on · 3 local agents",
            "⏵⏵ bypass permissions on · 1 shell, 1 monitor · ← 8 agents",
            "⏵⏵ bypass permissions on · 2 monitors",
            "⏵⏵ bypass permissions on · 3 background tasks · ← 8 agents",
            "⏵⏵ bypass permissions on · 1 team",
            "⏵⏵ bypass permissions on · 1 background dynamic workflow",
            "⏵⏵ bypass permissions on · 2 MCP tasks",
            "⏵⏵ bypass permissions on · ◇ 1 cloud session · ← 8 agents",
            "⏵⏵ bypass permissions on · ◇ 2 remote dynamic workflows",
            // 札が左側の最後で、右側の表示と空白だけで離れている
            "⏵⏵ bypass permissions on · 1 shell                    ✔Update installed · Restart to update",
            // 札が先頭
            "1 shell · ? for shortcuts",
            "⏸ plan mode on (shift+tab to cycle) · 12 shells",
        ] {
            let text = screen_with_footer("● started", footer);
            assert!(has_background_work(&text), "{footer}");
            assert_eq!(detect_status(&text), ClaudeStatus::Background, "{footer}");
        }
    }

    // 返答中 (スピナー行あり) は、裏の札があっても実行中が勝つ
    #[test]
    fn spinner_beats_background_pill() {
        let text = screen_with_footer(
            "✶ Boondoggling… (6s · ↓302 tokens)",
            "⏵⏵ bypass permissions on · 1 shell · ← 8 agents",
        );
        assert!(has_background_work(&text));
        assert_eq!(detect_status(&text), ClaudeStatus::Busy);
    }

    // 完了行の "1 shell still running" は会話に残る文字。フッターに札が無ければ
    // (= 裏の仕事は終わっている) こちらの番
    #[test]
    fn stale_still_running_line_is_not_background() {
        let text = screen_with_footer(
            "● started\n\n✻ Sautéed for 3s · done 18:08 · 1 shell still running",
            "⏵⏵ bypass permissions on (shift+tab to cycle) · ← 8 agents       /rc",
        );
        assert!(!has_background_work(&text));
        assert_eq!(detect_status(&text), ClaudeStatus::NeedsAttention);
    }

    // 本文 (罫線より上) にフッターそっくりの文字があっても当たらない
    #[test]
    fn pill_text_above_the_input_box_is_not_background() {
        let text = screen_with_footer(
            "フッターは次の形:\n  ⏵⏵ bypass permissions on · 1 shell · ← 8 agents",
            "⏵⏵ bypass permissions on · ← 8 agents",
        );
        assert!(!has_background_work(&text));
    }

    // 札ではないもの: ずっと出る見張り・本体の後片付け・エージェント一覧の案内・
    // 完全一致しない文字
    #[test]
    fn non_pill_footer_text_is_not_background() {
        for footer in [
            "⏵⏵ bypass permissions on · 1 Artifact comment monitor",
            "⏵⏵ bypass permissions on · saving memories",
            "⏵⏵ bypass permissions on · dreaming",
            "⏵⏵ bypass permissions on · ← 8 agents",
            "⏵⏵ bypass permissions on · 8 agents",
            "⏵⏵ bypass permissions on · 1 shell still running",
            "⏵⏵ bypass permissions on · shell",
            "⏵⏵ bypass permissions on · 1 shellfish",
            "⏵⏵ bypass permissions on · v1 shell",
            "⏵⏵ bypass permissions on · 1 shell, done",
            "⏵⏵ bypass permissions on",
            "",
        ] {
            let text = screen_with_footer("● started", footer);
            assert!(!has_background_work(&text), "{footer}");
            assert_eq!(
                detect_status(&text),
                ClaudeStatus::NeedsAttention,
                "{footer}"
            );
        }
    }

    // 入力欄 (罫線) が無い画面 = 許可ダイアログなど。札の文字があっても見ない
    #[test]
    fn no_input_box_is_not_background() {
        for text in [
            "Do you want to proceed?\n❯ 1. Yes\n  2. No\n  ⏵⏵ bypass permissions on · 1 shell",
            "a ─ b\n  1 shell",
            "a ── b\n  1 shell",
        ] {
            assert!(!has_background_work(text), "{text}");
            assert_eq!(detect_status(text), ClaudeStatus::NeedsAttention, "{text}");
        }
    }

    // 下枠が幅いっぱいで、フッターの行と改行なしでつながっていても拾う
    #[test]
    fn rule_joined_with_footer_line_is_background() {
        let rule = "─".repeat(72);
        let text =
            format!("● started\n{rule}❯ \n{rule}  ⏵⏵ bypass permissions on · 1 shell · ← 8 agents");
        assert_eq!(detect_status(&text), ClaudeStatus::Background);
    }

    // 上枠の右端にセッション名の札が付いていても、下枠より後ろだけを見る
    // (2026-11-14 のゆうさんのスクショの形)
    #[test]
    fn labelled_top_rule_does_not_hide_the_footer() {
        let rule = "─".repeat(72);
        let top = format!("{} tune-question-intonation-samples ─", "─".repeat(40));
        let footer = "  ⏵⏵ bypass permissions on · 1 shell · ⧉ index · ← 8 agents          ✔Update installed · Restart to update";
        let text = format!(
            "✻ Crunched for 1m 25s · done 17:44 · 1 shell still running\n\n{top}\n❯ \n{rule}\n{footer}\n{:>70}\n",
            "/rc"
        );
        assert_eq!(detect_status(&text), ClaudeStatus::Background);
    }

    // ---- is_waiting_for_background / 裏のエージェント待ち ----

    // 2026-11-15 のゆうさんのスクショの形: フッターに札が無く、入力欄のすぐ上が
    // 待ちの行。上枠の右端にセッション名の札、フッターの下にエージェントの一覧。
    fn screen_waiting(last_lines: &str, footer: &str) -> String {
        let rule = "─".repeat(72);
        let top = format!("{} resume-p1-affilia-features ─", "─".repeat(44));
        format!("● 続きを再開したよ。\n\n{last_lines}\n\n{top}\n❯ \n{rule}\n{footer}\n")
    }

    const AGENT_LIST_FOOTER: &str = "  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← 8 agents          /rc\n\n  ● main\n  ○ general-purpose  Probing node:sqlite in probe.mjs      10m 31s · ↓ 137.1k tokens";

    // 2026-11-15 に使い捨て ccnest の CCNEST_STATUS_TRACE で取った実物 (claude v2.1.325・
    // 裏のエージェントに sleep 60 をさせて返答を終えた直後・幅 100 桁)。行末の空白だけ
    // 落とした。プロンプトの後ろは NBSP、一覧の丸は ◯ (U+25EF)。フッターに札は無い。
    const REAL_WAITING_AGENT_SCREEN: &str = "\
❯ Use the Agent tool with run_in_background set to true to start one general-purpose agent. Its       only job is to run the shell command sleep 60 once and then answer done. Do not wait for it and
  do not check on it. Then reply with only the word started.

● Agent(Run sleep 60 once)
  ⎿ \u{a0}Backgrounded agent (↓ to manage · ctrl+o to expand)

● started

✻ Waiting for 1 background agent to finish

────────────────────────────────────────────────────────────────────────────────────────────────────
❯\u{a0}
────────────────────────────────────────────────────────────────────────────────────────────────────
  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← 8 agents
                             You've used 94% of your Fable limit · resets Nov 17, 3am (Asia/Tokyo)
                                                                                               /rc

  ● main
  ◯ general-purpose  Run sleep 60 once                                         2s · ↓ 48.6k tokens
";

    // 実物: 返答は終わり、裏のエージェントが動いている → 裏で実行中
    #[test]
    fn real_screen_waiting_for_background_agent_is_background() {
        assert!(!has_background_work(REAL_WAITING_AGENT_SCREEN));
        assert!(is_waiting_for_background(REAL_WAITING_AGENT_SCREEN));
        assert_eq!(
            detect_status(REAL_WAITING_AGENT_SCREEN),
            ClaudeStatus::Background
        );
    }

    // 同じ流れで、エージェントが終わって通知の返答も終わったあとの実物 (返答の
    // 本文は途中を省いた)。古い待ちの行が画面の上に残っている。下枠はフッターの
    // 行と改行なしでつながり、フッターに札は無い (`/tasks to see subagents` は案内)。
    const REAL_AGENT_DONE_SCREEN: &str = "\
✻ Waiting for 1 background agent to finish

● Agent \"Run sleep 60 once\" finished · 4s

● ゆうさん、さっきのエージェントから結果が返ってきたよ。

✻ Churned for 15s · done 16:34

────────────────────────────────────────────────────────────────────────────────────────────────────❯\u{a0}
────────────────────────────────────────────────────────────────────────────────────────────────────  ⏵⏵ bypass permissions on (shift+tab to cycle) · /tasks to see subagents · ← 8 agents         /rc
";

    // 実物: エージェントが終わったら、古い待ちの行が残っていてもこちらの番に戻る
    #[test]
    fn real_screen_after_background_agent_finished_is_attention() {
        assert!(REAL_AGENT_DONE_SCREEN.contains("Waiting for 1 background agent to finish"));
        assert!(!is_waiting_for_background(REAL_AGENT_DONE_SCREEN));
        assert!(!has_background_work(REAL_AGENT_DONE_SCREEN));
        assert_eq!(
            detect_status(REAL_AGENT_DONE_SCREEN),
            ClaudeStatus::NeedsAttention
        );
    }

    // 返答は終わり、入力欄のすぐ上が「Waiting for 1 background agent to finish」→ 裏で実行中
    #[test]
    fn waiting_line_above_the_input_box_is_background() {
        let text = screen_waiting(
            "✻ Waiting for 1 background agent to finish",
            AGENT_LIST_FOOTER,
        );
        assert!(!has_background_work(&text));
        assert!(is_waiting_for_background(&text));
        assert_eq!(detect_status(&text), ClaudeStatus::Background);
    }

    // 文言の組み立て (本体 v2.1.325): エージェントだけ / workflow だけ / 両方 (and)。
    // 先頭の記号は無くても、別の記号でもよい
    #[test]
    fn waiting_line_variants_are_background() {
        for line in [
            "✻ Waiting for 1 background agent to finish",
            "✻ Waiting for 3 background agents to finish",
            "✻ Waiting for 1 dynamic workflow to finish",
            "✻ Waiting for 2 dynamic workflows to finish",
            "✻ Waiting for 2 background agents and 1 dynamic workflow to finish",
            "* Waiting for 12 background agents to finish",
            "Waiting for 1 background agent to finish",
            "✻ Waiting for 1 background agent to finish   ",
        ] {
            let text = screen_waiting(line, "  ⏵⏵ bypass permissions on · ← 8 agents");
            assert!(is_waiting_for_background(&text), "{line}");
            assert_eq!(detect_status(&text), ClaudeStatus::Background, "{line}");
        }
    }

    // 古い待ちの行の下に別の内容が足されていれば (= エージェントは終わって次の
    // ターンが済んだ)、こちらの番
    #[test]
    fn stale_waiting_line_followed_by_more_output_is_attention() {
        let text = screen_waiting(
            "✻ Waiting for 1 background agent to finish\n\n● 担当の作業が終わったよ。\n\n✻ Worked for 12s · done 15:02",
            "  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← 8 agents",
        );
        assert!(!is_waiting_for_background(&text));
        assert_eq!(detect_status(&text), ClaudeStatus::NeedsAttention);
    }

    // 似た文は当たらない (完全に一致する形だけ)
    #[test]
    fn similar_lines_are_not_waiting() {
        for line in [
            "Waiting for the agent",
            "✻ Waiting for 1 background agent to finish soon",
            "✻ Waiting for background agent to finish",
            "✻ Waiting for 1 background agent",
            "✻ Waiting for 1 agent to finish",
            "✻ Waiting for 1 shell to finish",
            "✻ Waiting for 1 background agent and to finish",
            "✻ Waiting for  to finish",
            "● Waiting for 1 background agent to finish だよ",
            "✻ Worked for 9s · done 10:09",
        ] {
            let text = screen_waiting(line, "  ⏵⏵ bypass permissions on · ← 8 agents");
            assert!(!is_waiting_for_background(&text), "{line}");
            assert_eq!(detect_status(&text), ClaudeStatus::NeedsAttention, "{line}");
        }
    }

    // 返答中 (スピナー行あり) は実行中が勝つ
    #[test]
    fn spinner_beats_waiting_line() {
        let text = screen_waiting(
            "✻ Waiting for 1 background agent to finish\n\n❯ 進み具合は？\n\n✶ Boondoggling… (6s · ↓302 tokens)",
            AGENT_LIST_FOOTER,
        );
        assert_eq!(detect_status(&text), ClaudeStatus::Busy);
    }

    // 入力欄が無い画面 (許可ダイアログなど) は見ない
    #[test]
    fn waiting_line_without_input_box_is_not_background() {
        for text in [
            "✻ Waiting for 1 background agent to finish\n\nDo you want to proceed?\n❯ 1. Yes\n  2. No",
            "✻ Waiting for 1 background agent to finish",
            // 罫線が 1 本だけ (上枠が無い)
            "✻ Waiting for 1 background agent to finish\n────────\n  footer",
        ] {
            assert!(!is_waiting_for_background(text), "{text}");
        }
    }

    // 上枠が幅いっぱいで、プロンプトの行と改行なしでつながっていても拾う
    #[test]
    fn waiting_line_with_rule_joined_to_prompt_is_background() {
        let rule = "─".repeat(72);
        let text = format!(
            "● started\n\n✻ Waiting for 1 background agent to finish\n\n{rule}❯ \n{rule}  ⏵⏵ bypass permissions on · ← 8 agents\n"
        );
        assert_eq!(detect_status(&text), ClaudeStatus::Background);
    }

    // フッターの下の一覧に `─` が混ざっても、入力欄の枠を見失わない
    #[test]
    fn stray_rule_char_in_the_footer_does_not_hide_the_input_box() {
        let text = screen_waiting(
            "✻ Waiting for 1 background agent to finish",
            "  ⏵⏵ bypass permissions on · ← 8 agents\n\n  ● main\n  ○ general-purpose  a ─ b      5s · ↓ 1k tokens",
        );
        assert_eq!(detect_status(&text), ClaudeStatus::Background);
        let text = screen_with_footer(
            "● started",
            "⏵⏵ bypass permissions on · 1 shell\n\n  ○ general-purpose  a ─ b",
        );
        assert_eq!(detect_status(&text), ClaudeStatus::Background);
    }

    // ---- has_running_agent_row / フッターの下の一覧に動いているエージェントの行 ----

    // 2026-11-24 に使い捨て ccnest の CCNEST_STATUS_TRACE で取った実物 (claude v2.1.331・
    // 幅 120 桁・scripts/e2e/agent-list-real.ps1)。裏のエージェントが動いている間のフッターと
    // 一覧。丸は ◯ (U+25EF)、経過時間は 1 分を超えると `1m 1s`。
    const REAL_AGENT_LIST_FOOTER: &str = "\
  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← 8 agents · ↓ to manage                                       /rc

  ● main
  ◯ general-purpose  Running ping loopback test in foreground                                 1m 1s · ↓ 47.7k tokens";

    // エージェントが終わった直後 (通知の返答中) と、そのあとの実物のフッター。
    // エージェントが終わると一覧ごと消える。
    const REAL_AGENT_GONE_FOOTER: &str = "\
  ⏵⏵ bypass permissions on (shift+tab to cycle) · /tasks to see subagents · ← 8 agents                           /rc";

    // 2026-11-24 のゆうさんのスクショの書き起こし (実物の trace ではない): 返答は終わり、
    // 裏のエージェントが動いているが、入力欄の上に待ちの行 (`✻ Waiting for …`) が無い。
    // フッターに札も無い。目印はフッターの下の一覧の行だけ。上枠の右端にセッション名。
    fn screenshot_agent_running_without_waiting_line() -> String {
        let rule = "─".repeat(100);
        let top = format!("{} setup-staging-environment-youtube ─", "─".repeat(70));
        format!(
            "\
● ■ 残りの作業

  ・結果待ち: Google の「本番環境」への切り替え
  ・前からの判断待ち: 管理者編の動画の試写、手順書と yuya さん向け画像 2 枚を直すかどうか

  切り替えの結果が来たら、すぐ知らせるね✨

{top}
❯\u{a0}
{rule}
  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← 8 agents · ↓ to manage                    /rc

  ● main
  ○ general-purpose  ブランディング設定を保存中                          2m 17s · ↓ 116.0k tokens
"
        )
    }

    // スクショの画面: 待ちの行も札も無いが、一覧に動いている行がある → 裏で実行中
    // (v0.1.53 までは NeedsAttention = 🟨 だった)
    #[test]
    fn running_agent_row_without_waiting_line_is_background() {
        let text = screenshot_agent_running_without_waiting_line();
        assert!(!has_background_work(&text));
        assert!(!is_waiting_for_background(&text));
        assert!(has_running_agent_row(&text));
        assert_eq!(detect_status(&text), ClaudeStatus::Background);
    }

    // 実物のフッター: 動いている間は行がある / 終わったら一覧ごと消える
    #[test]
    fn real_agent_list_footer_is_background_only_while_the_agent_runs() {
        let running = screen_waiting("● mango", REAL_AGENT_LIST_FOOTER);
        assert!(!is_waiting_for_background(&running));
        assert!(has_running_agent_row(&running));
        assert_eq!(detect_status(&running), ClaudeStatus::Background);

        let gone = screen_waiting("● mango", REAL_AGENT_GONE_FOOTER);
        assert!(!has_running_agent_row(&gone));
        assert_eq!(detect_status(&gone), ClaudeStatus::NeedsAttention);
        // 一覧が無い実物 (エージェントが終わったあと) は今までどおりこちらの番
        assert!(!has_running_agent_row(REAL_AGENT_DONE_SCREEN));
        // 待ちの行と一覧の両方がある実物も、もちろん裏で実行中
        assert!(has_running_agent_row(REAL_WAITING_AGENT_SCREEN));
    }

    // 行の形: 経過時間 (秒 / 分秒 / 時分秒) + ` · ↓ <数> tokens` で終わる
    #[test]
    fn running_agent_row_shapes() {
        for row in [
            "  ◯ general-purpose  Run ping command                2s · ↓ 47.7k tokens",
            "  ○ general-purpose  ブランディング設定を保存中      2m 17s · ↓ 116.0k tokens",
            "  ◯ general-purpose  Scheduling final run       1h 1m 46s · ↓ 440.5k tokens",
            "  ◯ Explore  Find the footer code                    12s · ↓ 302 tokens",
            "  ◯ general-purpose  a  5s · ↓ 1.2m tokens   ",
            "  ◯ general-purpose  a  5s · ↓1k tokens",
            // 丸が別の記号 (選んでいる行) でも、説明が数字で終わっていてもよい
            "  ❯ general-purpose  Retry step 3                    5s · ↓ 1k tokens",
        ] {
            assert!(is_running_agent_row(row), "{row}");
            let text = screen_waiting("● started", &format!("  ⏵⏵ bypass\n\n  ● main\n{row}"));
            assert_eq!(detect_status(&text), ClaudeStatus::Background, "{row}");
        }
    }

    // フッターのほかの行は当たらない
    #[test]
    fn other_footer_lines_are_not_running_agent_rows() {
        for line in [
            "  ● main",
            "  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← 8 agents · ↓ to manage        /rc",
            "  ⏵⏵ bypass permissions on (shift+tab to cycle) · /tasks to see subagents · ← 8 agents",
            "       You've used 94% of your Fable limit · resets Nov 17, 3am (Asia/Tokyo)",
            // 出だしの 1 秒 (トークン数がまだ無い)。この瞬間は返答中 (Busy) なので拾わなくてよい
            "  ◯ general-purpose  Run ping command                                    0s",
            // 経過時間が無い / トークン数が無い / 形が違う
            "  ◯ general-purpose  Run ping command   · ↓ 47.7k tokens",
            "  ◯ general-purpose  Run ping command   done · ↓ 47.7k tokens",
            "  ◯ general-purpose  Run ping command   2s · ↓ tokens",
            "  ◯ general-purpose  Run ping command   2s · ↓ many tokens",
            "  ◯ general-purpose  Run ping command   2s · 47.7k tokens",
            "  ◯ general-purpose  Run ping command   2s ↓ 47.7k tokens",
            "  ◯ general-purpose  Run ping command   2s · ↓ 47.7k tokens used",
            "",
        ] {
            assert!(!is_running_agent_row(line), "{line}");
            let text = screen_waiting("● started", &format!("  ⏵⏵ bypass\n{line}"));
            assert_eq!(detect_status(&text), ClaudeStatus::NeedsAttention, "{line}");
        }
    }

    // 会話の本文 (入力欄より上) に同じ形の行があっても当たらない
    #[test]
    fn agent_row_text_in_the_transcript_is_not_background() {
        let text = screen_waiting(
            "  ◯ general-purpose  Run ping command        2s · ↓ 47.7k tokens\n\n● 一覧はこう見えるよ。",
            "  ⏵⏵ bypass permissions on · ← 8 agents",
        );
        assert!(!has_running_agent_row(&text));
        assert_eq!(detect_status(&text), ClaudeStatus::NeedsAttention);
        // 入力欄が無い画面 (許可ダイアログなど) は見ない
        assert!(!has_running_agent_row(
            "Do you want to proceed?\n  ◯ general-purpose  a   2s · ↓ 1k tokens"
        ));
    }

    // 返答中 (スピナー行あり) は実行中が勝つ
    #[test]
    fn spinner_beats_running_agent_row() {
        let text = screen_waiting(
            "❯ 進み具合は？\n\n✶ Boondoggling… (6s · ↓302 tokens)",
            REAL_AGENT_LIST_FOOTER,
        );
        assert_eq!(detect_status(&text), ClaudeStatus::Busy);
    }

    // 一覧に動いている行があっても、返答が「判断待ち」(中身あり) で終わっていればこちらの番
    #[test]
    fn decision_pending_beats_running_agent_row() {
        let text = screen_waiting(
            "▼ ゆうさんの判断待ち\n・マーク: A か B のどれにするか",
            REAL_AGENT_LIST_FOOTER,
        );
        assert!(has_running_agent_row(&text));
        assert_eq!(detect_status(&text), ClaudeStatus::NeedsAttention);
        let none = screen_waiting("▼ ゆうさんの判断待ち\n・今はなし", REAL_AGENT_LIST_FOOTER);
        assert_eq!(detect_status(&none), ClaudeStatus::Background);
    }

    // 入力欄が空でも、一覧に動いている行があれば落とさない (落とすと裏のエージェントが止まる)
    #[test]
    fn running_agent_row_is_not_safe_to_stop() {
        let text = screenshot_agent_running_without_waiting_line();
        assert!(at_empty_prompt(&text));
        assert!(!is_safe_to_stop(&text));
        // 同じ画面でエージェントが終わっていれば (一覧が無ければ) 落としてよい
        let done = screen_waiting("● 終わったよ。", REAL_AGENT_GONE_FOOTER);
        assert!(is_safe_to_stop(&done));
    }

    // ---- 放置中のまとめ (idle compaction) が待ちの行の下に足した行 ----

    const IDLE_NOTICE: &str = "● Compacted while idle, before the prompt cache expired · turn off with \"idleCompaction\": false in settings.json";

    // 待ちの行と、その子の行 (まとめのあとに読み直した物の一覧)。
    const WAITING_WORKFLOW_WITH_CHILDREN: &str = "\
✻ Waiting for 1 dynamic workflow to finish
  ⎿  Referenced file ..\\28_Affilia\\CLAUDE.md
  ⎿  Read ..\\..\\..\\.claude\\projects\\C--Users-mitam-Desktop-work-1-own-10-ops-10-ClaudeCompany\\memory\\project_affilia.md (39 lines)
  ⎿  Read ..\\28_Affilia\\apps\\renderer\\src\\menu.tsx (65 lines)
  ⎿  Read ..\\..\\..\\.claude\\skills\\chrome-browser-testing\\SKILL.md (195 lines)
  ⎿  Referenced file outputs\\20_xtp3\\2026-10-23_xboard_research\\report.md
  ⎿  Plan file referenced (~\\.claude\\plans\\affilia-xboard-ui-ux-ultracode-zesty-sloth.md)
  ⎿  Skills restored (deep-plan, workflow-authoring)";

    /// 2026-11-18 のゆうさんのスクショの形 (claude v2.1.327): 返答の終わりのあとに
    /// `tail` (待ちの行・お知らせなど) が続き、フッターの下に動いている workflow の行。
    /// フッターに札は無い。**スクショからの書き起こし**で、CCNEST_STATUS_TRACE の実物ではない
    /// (workflow を動かしたまま 1 時間放置しないと出ない画面)。
    fn screen_idle_compacted(tail: &str) -> String {
        let rule = "─".repeat(120);
        format!(
            "■ 2. 残りの作業\n\n  ・レイナの残り: 調査（裏で実行中）が終わったら、差分表を出して計画に書く\n  ・ゆうさんの判断待ち: 今は無し\n\n  effort: max（deep-plan）\n\n{tail}\n\n{rule}\n❯\u{a0}\n{rule}\n  ⏸ plan mode on (shift+tab to cycle) · ← 8 agents                    ✔Update installed · Restart to update\n{:>118}\n\n  ◯ affilia-xboard-gap  ▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▭▭▭  8/9 · 2h05m · ↓ 1.6m tokens · ⚠Large workflow · /workflows to stop\n",
            "/rc"
        )
    }

    // スクショの画面: 待ちの行の下に子の行とお知らせが足されていても、裏で実行中
    #[test]
    fn waiting_line_followed_by_idle_compaction_notice_is_background() {
        let text = screen_idle_compacted(&format!(
            "{WAITING_WORKFLOW_WITH_CHILDREN}\n\n{IDLE_NOTICE}"
        ));
        assert!(!has_background_work(&text));
        assert!(!asks_for_decision(&text));
        assert!(is_waiting_for_background(&text));
        assert_eq!(detect_status(&text), ClaudeStatus::Background);
    }

    // お知らせが無く、子の行だけが足されていても同じ
    #[test]
    fn waiting_line_followed_only_by_child_lines_is_background() {
        let text = screen_idle_compacted(WAITING_WORKFLOW_WITH_CHILDREN);
        assert!(is_waiting_for_background(&text));
        assert_eq!(detect_status(&text), ClaudeStatus::Background);
    }

    // 狭いペイン: お知らせも子の行も折り返す (続きの行は字下げされる)
    #[test]
    fn wrapped_notice_and_wrapped_child_lines_are_background() {
        let text = screen_idle_compacted(
            "✻ Waiting for 1 dynamic workflow to finish\n  ⎿  Read ..\\..\\..\\.claude\\projects\\C--Users-mitam-Desktop-work-1-own-10-ops-10-ClaudeCompany\\memory\\\n     project_affilia.md (39 lines)\n  ⎿  Skills restored (deep-plan, workflow-authoring)\n\n● Compacted while idle, before the prompt cache expired · turn off with\n  \"idleCompaction\": false in settings.json",
        );
        assert!(is_waiting_for_background(&text));
        assert_eq!(detect_status(&text), ClaudeStatus::Background);
    }

    // お知らせのもう 1 つの文言 (本体 v2.1.327)・エージェント待ち・お知らせが 2 つ続く形
    #[test]
    fn idle_compaction_notice_variants_are_background() {
        for tail in [
            "✻ Waiting for 1 dynamic workflow to finish\n\n● Compacted using a summary written while you were away · turn off with \"idleCompaction\": false in settings.json".to_string(),
            format!("✻ Waiting for 2 background agents to finish\n  ⎿  Read a.md (3 lines)\n\n{IDLE_NOTICE}"),
            format!("✻ Waiting for 1 dynamic workflow to finish\n\n{IDLE_NOTICE}\n\n{IDLE_NOTICE}"),
            "✻ Waiting for 1 dynamic workflow to finish\n\n* Compacted while idle, before the prompt cache expired".to_string(),
        ] {
            let text = screen_idle_compacted(&tail);
            assert!(is_waiting_for_background(&text), "{tail}");
            assert_eq!(detect_status(&text), ClaudeStatus::Background, "{tail}");
        }
    }

    // お知らせの下に次のターンが足されていれば (= もう待っていない)、こちらの番
    #[test]
    fn new_turn_below_the_idle_compaction_notice_is_attention() {
        for after in [
            "❯ 進み具合は？",
            "● Dynamic workflow \"affilia-xboard-gap\" completed\n\n● 調査が終わったよ。\n\n✻ Churned for 15s · done 16:34",
            "✻ Worked for 12s · done 15:02",
        ] {
            let text = screen_idle_compacted(&format!(
                "{WAITING_WORKFLOW_WITH_CHILDREN}\n\n{IDLE_NOTICE}\n\n{after}"
            ));
            assert!(!is_waiting_for_background(&text), "{after}");
            assert_eq!(detect_status(&text), ClaudeStatus::NeedsAttention, "{after}");
        }
    }

    // 待ちの行が無い (ふつうに終わった返答のあとのまとめ)・お知らせに似ているだけの行・
    // お知らせと待ちの行のあいだに別の行、はこちらの番
    #[test]
    fn idle_compaction_notice_without_waiting_line_is_attention() {
        for tail in [
            format!("● 終わったよ。\n\n✻ Worked for 12s · done 15:02\n\n{IDLE_NOTICE}"),
            IDLE_NOTICE.to_string(),
            "✻ Waiting for 1 dynamic workflow to finish\n\n● Compacted だよ".to_string(),
            "✻ Waiting for 1 dynamic workflow to finish\n\n● Conversation compacted (ctrl+o for history)".to_string(),
            format!("✻ Waiting for 1 dynamic workflow to finish\n\n● 途中の知らせ\n\n{IDLE_NOTICE}"),
        ] {
            let text = screen_idle_compacted(&tail);
            assert!(!is_waiting_for_background(&text), "{tail}");
            assert_eq!(detect_status(&text), ClaudeStatus::NeedsAttention, "{tail}");
        }
    }

    // 判断待ちに中身があれば、待ちの行とお知らせで終わっていてもこちらの番 (v0.1.42 のまま)
    #[test]
    fn decision_items_before_idle_compaction_notice_is_attention() {
        let text = screen_idle_compacted(&format!(
            "▼ ゆうさんの判断待ち\n・A と B のどちらにするか\n\n{WAITING_WORKFLOW_WITH_CHILDREN}\n\n{IDLE_NOTICE}"
        ));
        assert!(is_waiting_for_background(&text));
        assert!(asks_for_decision(&text));
        assert_eq!(detect_status(&text), ClaudeStatus::NeedsAttention);
    }

    // ---- screen_text / 行ごとに取った画面を判定用の文字にする ----

    // 行は必ず改行で区切る。行末の空白と、末尾の空行は落とす
    #[test]
    fn screen_text_joins_rows_with_newlines_and_trims() {
        let rows = ["● ▼ 見出し      ", "  ・項目", "", "❯ ", "   ", ""].map(String::from);
        assert_eq!(screen_text(rows), "● ▼ 見出し\n  ・項目\n\n❯");
        assert_eq!(screen_text(Vec::<String>::new()), "");
    }

    // vt100 の contents() は「最終列まで埋まった行」を次の行と改行なしでつなぐ。
    // 2026-11-15 の実物: 見出しの行が空白で最終列まで埋まり、項目の行とつながって
    // 1 行になった (判定が外れて Background のままだった)。行ごとに取れば分かれる
    #[test]
    fn rows_keep_heading_and_item_apart_where_contents_joined_them() {
        let heading = format!("● ▼ ゆうさんの判断待ち{}", " ".repeat(78));
        let rule = "─".repeat(100);
        let footer = "  ⏵⏵ bypass permissions on · 1 shell · ← 9 agents      /rc";
        // contents() の形 (つながっている) では見出しを見つけられない
        let joined = format!(
            "{heading}・マーク: A か B のどれにするか\n\n✻ Churned for 5s · done 17:06 · 1 shell still running\n\n{rule}\n❯\u{a0}\n{rule}\n{footer}"
        );
        assert!(!asks_for_decision(&joined));
        // 行ごとの形
        let rows = [
            heading.as_str(),
            "  ・マーク: A か B のどれにするか",
            "",
            "✻ Churned for 5s · done 17:06 · 1 shell still running",
            "",
            rule.as_str(),
            "❯\u{a0}",
            rule.as_str(),
            footer,
        ]
        .map(String::from);
        let text = screen_text(rows);
        assert!(asks_for_decision(&text));
        assert_eq!(detect_status(&text), ClaudeStatus::NeedsAttention);
    }

    // ---- asks_for_decision / 裏で動いていても判断待ちならこちらの番 ----

    const SHELLS_FOOTER: &str = "  ⏵⏵ bypass permissions on · 2 shells · ← 8 agents          /rc\n\n  ● main\n  ◯ general-purpose  Scheduling final e2e-live.mjs run      1h 1m 46s · ↓ 440.5k tokens";

    // 2026-11-15 のゆうさんの 2 枚目のスクショの形: 裏でシェル 2 つとエージェントが
    // 動いているが、返答の最後の「ゆうさんの判断待ち」に中身がある → こちらの番
    #[test]
    fn decision_items_with_background_work_is_attention() {
        let text = screen_waiting(
            "▼ 裏で動いているもの\n・P1 の担当が 1 つ。最後の通し検査の途中（完了の知らせはまだ）\n\n▼ レイナの残り\n・ロゴが決まったら、P1 のあとにアプリへ表示\n・そのあと P2〜P10 を順番に\n\n▼ ゆうさんの判断待ち\n・マーク: A〜F のどれにするか\n・文字: 1／2／3 のどれにするか\n\n✻ Crunched for 1m 27s · done 16:59 · 2 shells still running",
            SHELLS_FOOTER,
        );
        assert!(has_background_work(&text));
        assert!(asks_for_decision(&text));
        assert_eq!(detect_status(&text), ClaudeStatus::NeedsAttention);
    }

    // 2026-11-15 に使い捨て ccnest の CCNEST_STATUS_TRACE で取った実物 (claude v2.1.325・
    // 行ごとに取った形)。裏で ping を走らせたまま、返答が判断待ちの節で終わっている。
    // 見出しの前に返答の印 `● ` が付き、項目は 2 桁の字下げ。フッターに `1 shell` の札。
    const REAL_DECISION_SCREEN: &str = "\
● Bash(ping -n 150 127.0.0.1)
  ⎿ \u{a0}Running in the background (↓ to manage)

  Read 1 file (ctrl+o to expand)

● ▼ ゆうさんの判断待ち
  ・マーク: A か B のどれにするか

✻ Baked for 5s · done 17:09 · 1 shell still running

────────────────────────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────────────────────────
  ⏵⏵ bypass permissions on · 1 shell · ← 8 agents                                              /rc";

    // 実物: 裏のシェルが動いていても、判断待ちに中身があればこちらの番
    #[test]
    fn real_screen_decision_pending_with_background_shell_is_attention() {
        assert!(has_background_work(REAL_DECISION_SCREEN));
        assert!(asks_for_decision(REAL_DECISION_SCREEN));
        assert_eq!(
            detect_status(REAL_DECISION_SCREEN),
            ClaudeStatus::NeedsAttention
        );
        // 同じ画面で項目だけ「今はなし」なら裏で実行中 (実機の 2 つ目の返答と同じ形)
        let none = REAL_DECISION_SCREEN.replace("マーク: A か B のどれにするか", "今はなし");
        assert_eq!(detect_status(&none), ClaudeStatus::Background);
    }

    // 1 枚目のスクショの形: 判断待ちが「今はなし」→ 裏で実行中のまま
    #[test]
    fn decision_none_with_background_work_stays_background() {
        let text = screen_waiting(
            "▼ レイナの残り\n・P1 の結果を確かめて、P2〜P10 を順番に進める\n\n▼ ゆうさんの判断待ち\n・今はなし\n\n※裏の担当は、このセッションを閉じると一緒に止まる。\n\n✻ Waiting for 1 background agent to finish",
            AGENT_LIST_FOOTER,
        );
        assert!(!asks_for_decision(&text));
        assert_eq!(detect_status(&text), ClaudeStatus::Background);
    }

    // 「なし」の書き方いろいろ。長い「なし」が折り返して 2 行になっても中身なし
    #[test]
    fn decision_none_wordings_are_not_a_decision() {
        for body in [
            "・なし",
            "なし",
            "・今のところなし",
            "・特になし",
            "- 無し",
            "・ありません",
            "・None",
            "・今はなし（裏の担当が終わったら\n  レイナが続きを進める）",
        ] {
            let text = screen_waiting(
                &format!("▼ ゆうさんの判断待ち\n{body}\n\n✻ Cooked for 3s · done 18:08"),
                SHELLS_FOOTER,
            );
            assert!(!asks_for_decision(&text), "{body}");
            assert_eq!(detect_status(&text), ClaudeStatus::Background, "{body}");
        }
    }

    // 見出しの書き方いろいろ (記号・通し番号・末尾のコロン・記号なし)
    #[test]
    fn decision_heading_variants_are_attention() {
        for heading in [
            "▼ ゆうさんの判断待ち",
            "■ 5. ゆうさんの判断待ち",
            "ゆうさんの判断待ち",
            "▼ ゆうさんの判断待ち：",
            "▼ 判断待ち:",
        ] {
            let text = screen_waiting(
                &format!("{heading}\n・A と B のどちらにするか\n\n✻ Cooked for 3s · done 18:08"),
                SHELLS_FOOTER,
            );
            assert!(asks_for_decision(&text), "{heading}");
            assert_eq!(
                detect_status(&text),
                ClaudeStatus::NeedsAttention,
                "{heading}"
            );
        }
    }

    // 判断待ちのあとにユーザーの発話 (❯ …) があれば、もう答えている
    #[test]
    fn decision_answered_by_the_user_is_not_a_decision() {
        let text = screen_waiting(
            "▼ ゆうさんの判断待ち\n・マーク: A〜F のどれにするか\n\n✻ Crunched for 1m 27s · done 16:59 · 2 shells still running\n\n❯ D と 3 で\n\n● 了解、D と 3 で進めるね。\n\n✻ Cooked for 3s · done 17:02 · 2 shells still running",
            SHELLS_FOOTER,
        );
        assert!(!asks_for_decision(&text));
        assert_eq!(detect_status(&text), ClaudeStatus::Background);
    }

    // 見出しでないものは当たらない: 中身の無い見出し・箇条書きの中の言葉・長い文
    #[test]
    fn non_headings_and_empty_sections_are_not_a_decision() {
        for body in [
            "▼ ゆうさんの判断待ち\n\n・これは別の段落",
            "▼ ゆうさんの判断待ち",
            "▼ レイナの残り\n・ロゴの反映はゆうさんの判断待ち\n・P2 を進める",
            "ここまでの作業は全部終わっていて、ロゴをどれにするかだけがゆうさんの判断待ち\n・A\n・B",
            "▼ 判断待ちの一覧は次のとおり\n・A",
        ] {
            let text = screen_waiting(
                &format!("{body}\n\n✻ Cooked for 3s · done 18:08"),
                SHELLS_FOOTER,
            );
            assert!(!asks_for_decision(&text), "{body}");
            assert_eq!(detect_status(&text), ClaudeStatus::Background, "{body}");
        }
    }

    // 返答中 (スピナー行あり) は、判断待ちが画面に残っていても実行中
    #[test]
    fn spinner_beats_decision_section() {
        let text = screen_waiting(
            "▼ ゆうさんの判断待ち\n・マーク: A〜F のどれにするか\n\n● Agent \"P1\" finished · 1h 2m\n\n✶ Boondoggling… (6s · ↓302 tokens)",
            SHELLS_FOOTER,
        );
        assert_eq!(detect_status(&text), ClaudeStatus::Busy);
    }

    // 入力欄が無い画面は見ない (どのみち NeedsAttention)
    #[test]
    fn decision_without_input_box_is_not_checked() {
        let text = "▼ ゆうさんの判断待ち\n・A か B か\n\nDo you want to proceed?\n❯ 1. Yes";
        assert!(!asks_for_decision(text));
        assert_eq!(detect_status(text), ClaudeStatus::NeedsAttention);
    }

    // ---- aggregate_status ----

    // 裏で実行中は shell より上、実行中・対応待ちより下 (順序非依存)
    #[test]
    fn aggregate_background_ranks_between_idle_and_busy() {
        assert_eq!(
            aggregate_status([ClaudeStatus::Idle, ClaudeStatus::Background]),
            ClaudeStatus::Background
        );
        assert_eq!(
            aggregate_status([ClaudeStatus::Busy, ClaudeStatus::Background]),
            ClaudeStatus::Busy
        );
        assert_eq!(
            aggregate_status([ClaudeStatus::Background, ClaudeStatus::Busy]),
            ClaudeStatus::Busy
        );
        assert_eq!(
            aggregate_status([ClaudeStatus::NeedsAttention, ClaudeStatus::Background]),
            ClaudeStatus::NeedsAttention
        );
    }

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

    // 裏で実行中も緑の丸 (実行中と同じ絵文字・ユーザー決定 2026-11-15)
    #[test]
    fn marker_background_is_green_circle_like_busy() {
        assert_eq!(status_marker(ClaudeStatus::Background), "🟢");
        assert_eq!(
            status_marker(ClaudeStatus::Background),
            status_marker(ClaudeStatus::Busy)
        );
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

    // どのマーカーも表示幅 2 (従来の 🟩 と同じ・タブ幅計算の前提)
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
// - ver1.12 (2026-11-24): has_running_agent_row を追加。裏のエージェントが動いているのに
//   入力欄の上に待ちの行 (`✻ Waiting for … to finish`) が出ないターンがあり、フッターに
//   札も無いので 🟨 のままだった (ゆうさんのスクショ・claude v2.1.331)。フッターの下の
//   一覧の行 `◯ general-purpose  … 2m 17s · ↓ 116.0k tokens` (経過時間 + トークン数で
//   終わる行) があれば Background。エージェントが終わると一覧ごと消える (実物で確認)。
//   is_safe_to_stop にも足した (保留にしても裏のエージェントを止めない)。
//   行の形は scripts/e2e/agent-list-real.ps1 で取った実物、待ちの行が無い画面は
//   スクショからの書き起こし
// - ver1.11 (2026-11-21): at_empty_prompt / is_safe_to_stop を追加。保留 (Alt+C) にした
//   タブの Claude を落としてメモリを空けてよい画面か = 実行中でない・裏の仕事が無い・
//   入力欄が空で出ている (ダイアログの途中・書きかけの入力は偽)。読めない画面は偽
//   (落とさない側に倒す)。
// - ver1.10 (2026-11-18): 放置中のまとめ (idle compaction) のあとも、裏の workflow /
//   エージェント待ちを Background のままにする。まとめはターンを始めずに、待ちの行の下へ
//   子の行 (`  ⎿  Referenced file …`) とお知らせ (`● Compacted while idle, …`) を足すので、
//   「最後の空でない行」が待ちの行でなくなり 🟨 に落ちていた (ゆうさんのスクショ)。
//   is_waiting_for_background は「0 桁目から始まる最後の行」を見て (子の行と折り返しの
//   続きは字下げされている)、お知らせ (IDLE_COMPACTION_NOTICES・claude v2.1.327 の本体で
//   確認) は読み飛ばす。テストの画面はスクショからの書き起こし
// - ver1.9 (2026-11-15): 裏の仕事が動いていても、返答が「判断待ち」の節 (中身あり) で
//   終わっていれば NeedsAttention (ユーザー決定「ゆうさんの判断待ちがあるときはオレンジ
//   アイコンが良い」)。asks_for_decision が会話の部分にある最後の見出し
//   (`▼ ゆうさんの判断待ち`) のすぐ下の行を見る: 「今はなし」の類・空行・見出しより
//   後ろのユーザー発話 (❯ …) は偽。会話の部分の切り出しを transcript に共用化。
//   screen_text を追加: 判定に渡す文字は vt100 の行を改行でつないで作る
//   (contents() は最終列まで埋まった行を次の行とつなぎ、見出しと項目が 1 行に
//   なって実機で判定が外れた)
// - ver1.8 (2026-11-15): 裏で実行中 (Background) のマーカーを 🔵 → 🟢 に (ユーザー決定
//   「やっぱり、青の判定時も緑アイコンにして欲しい」)。status_marker だけの変更で、
//   判定と畳み込みの順位 (NeedsAttention > Busy > Background > Idle) はそのまま
// - ver1.7 (2026-11-15): 裏のエージェント待ちも Background (🔵) にする。この場面は
//   フッターに札が出ず (裏のエージェントはフッターの下の一覧に出る)、返答が
//   "✻ Waiting for 1 background agent to finish" で終わるのに 🟨 だった。
//   is_waiting_for_background が「入力欄の上枠より前の、最後の空でない行」を見る
//   (会話に残る文字なので、下に別の内容が足された古い行には当たらない)。
//   罫線探しを last_rule に切り出し、「最後の `─`」ではなく「3 個以上並んだ最後の所」に
//   した (フッターの下の一覧に `─` が混ざっても入力欄を見失わない)。
//   テスト文字列は CCNEST_STATUS_TRACE で取った claude v2.1.325 の実物
// - ver1.6 (2026-11-14): 3 つ目の状態 Background (🔵 裏で実行中) を追加 (ユーザー決定)。
//   返答が終わるとスピナー行が消え、裏でシェルやエージェントが動いていても 🟨 に
//   なっていた。has_background_work がフッター (最後の罫線より後ろ) の札
//   "1 shell" / "2 local agents" / "3 background tasks" … を見る。完了行の
//   "· 1 shell still running" は会話に残る文字なので使わない。優先度は
//   NeedsAttention > Busy > Background > Idle。テスト文字列は CCNEST_STATUS_TRACE
//   (app.rs) で取った claude v2.1.325 の実物
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
