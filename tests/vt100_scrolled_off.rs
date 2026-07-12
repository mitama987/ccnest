//! vendored vt100 フォーク ([patch.crates-io]) に追加した
//! `total_scrolled_off` カウンタと deep-scrollback 読み出しの挙動テスト。
//!
//! ccnest の選択 (Selection) はバッファ絶対座標
//! `abs = total_scrolled_off - scrollback_offset + screen_y` で保持されるため、
//! このカウンタの単調性・alt 画面での恒常 0・スクロールバック追従 (glue) が
//! 選択の正しさの土台になる。

/// 画面 y 行のテキストを組み立てる (wide-continuation なし前提の簡易版)。
fn row_text(screen: &vt100::Screen, y: u16) -> String {
    let (_, cols) = screen.size();
    let mut s = String::new();
    for x in 0..cols {
        if let Some(cell) = screen.cell(y, x) {
            let c = cell.contents();
            if c.is_empty() {
                s.push(' ');
            } else {
                s.push_str(&c);
            }
        }
    }
    s.trim_end().to_string()
}

/// カウンタは push された行数を数え、キャップの追い出し (eviction) 後も単調のまま。
#[test]
fn counter_counts_pushes_and_survives_eviction() {
    let mut parser = vt100::Parser::new(3, 20, 4);
    let mut payload = String::new();
    for i in 1..=9 {
        payload.push_str(&format!("line{:02}\r\n", i));
    }
    payload.push_str("line10");
    parser.process(payload.as_bytes());
    // 10 行を 3 行グリッドに流すと 7 行がスクロールバックへ押し出される。
    assert_eq!(parser.screen().total_scrolled_off(), 7);
    // 容量 4 なので保持されているのは直近 4 行 (line04..line07) のみ。
    assert_eq!(parser.screen().scrollback_rows(), 4);
}

/// alt 画面ではカウンタが常に 0。primary 側のカウンタは alt 中も保持される。
#[test]
fn alt_screen_counter_is_zero_and_primary_preserved() {
    let mut parser = vt100::Parser::new(3, 20, 100);
    parser.process(b"a\r\nb\r\nc\r\nd\r\ne");
    let primary = parser.screen().total_scrolled_off();
    assert_eq!(primary, 2);

    // alt 画面へ (DECSET 1049)。alt grid は scrollback 容量 0。
    parser.process(b"\x1b[?1049h");
    assert!(parser.screen().alternate_screen());
    parser.process(b"x\r\ny\r\nz\r\nw\r\nv");
    assert_eq!(parser.screen().total_scrolled_off(), 0);

    // primary へ戻るとカウンタは保持されたまま。
    parser.process(b"\x1b[?1049l");
    assert!(!parser.screen().alternate_screen());
    assert_eq!(parser.screen().total_scrolled_off(), primary);
}

/// スクロールバック表示中 (offset > 0) に新規行が来ると offset が自動追従し、
/// viewport 先頭 abs (= total - offset) が不変 = 表示内容が動かない (glue)。
/// offset == 0 (最下部) では viewport 先頭 abs が行数ぶん進む。
#[test]
fn viewport_top_abs_glued_when_scrolled_back() {
    let mut parser = vt100::Parser::new(3, 20, 100);
    let mut payload = String::new();
    for i in 1..=6 {
        payload.push_str(&format!("l{}\r\n", i));
    }
    parser.process(payload.as_bytes());
    let total_before = parser.screen().total_scrolled_off();
    assert_eq!(total_before, 4);

    // 2 行分スクロールバックを表示した状態で新規行が届く。
    parser.set_scrollback(2);
    let top_before = total_before as i64 - 2;
    parser.process(b"l7\r\n");
    let s = parser.screen();
    assert_eq!(s.scrollback(), 3, "offset が自動で +1 追従する");
    assert_eq!(
        s.total_scrolled_off() as i64 - s.scrollback() as i64,
        top_before,
        "viewport 先頭 abs は不変 (表示が内容に張り付く)"
    );

    // 最下部 (offset 0) では新規行のたびに viewport 先頭 abs が進む。
    parser.set_scrollback(0);
    let total0 = parser.screen().total_scrolled_off();
    parser.process(b"l8\r\n");
    assert_eq!(parser.screen().scrollback(), 0);
    assert_eq!(parser.screen().total_scrolled_off(), total0 + 1);
}

/// RIS (ESC c) はグリッドと total_scrolled_off を作り直すため、reset_generation
/// が進む。abs 座標を持つ側 (ccnest の選択) はこれで座標系の断絶を検知できる。
#[test]
fn ris_bumps_reset_generation_and_resets_counter() {
    let mut parser = vt100::Parser::new(3, 20, 100);
    parser.process(b"a\r\nb\r\nc\r\nd\r\ne");
    assert_eq!(parser.screen().total_scrolled_off(), 2);
    assert_eq!(parser.screen().reset_generation(), 0);

    parser.process(b"\x1bc");
    assert_eq!(parser.screen().reset_generation(), 1);
    assert_eq!(parser.screen().total_scrolled_off(), 0);
    assert!(!parser.screen().alternate_screen());
}

/// 1 画面より深い scrollback offset でも可視セルが読める
/// (grid.rs visible_rows の usize アンダーフロー → saturating_sub 修正の回帰テスト。
/// debug ビルドで実行されることに意味がある)。
#[test]
fn deep_scrollback_offset_does_not_underflow() {
    let mut parser = vt100::Parser::new(3, 20, 100);
    let mut payload = String::new();
    for i in 1..=30 {
        payload.push_str(&format!("line{:02}\r\n", i));
    }
    parser.process(payload.as_bytes());
    assert_eq!(parser.screen().total_scrolled_off(), 28);

    // offset(25) > rows(3) の deep scrollback。y=0 は scrollback[28-25] = line04。
    parser.set_scrollback(25);
    assert_eq!(row_text(parser.screen(), 0), "line04");
}
