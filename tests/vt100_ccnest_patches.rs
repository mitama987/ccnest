//! ccnest が vendored vt100 に当てた小パッチの回帰テスト。
//!
//! - `Cell::write_contents`: 描画ループでセルごとに String を確保しないための
//!   追記版。`contents()` と同じ文字列を出すこと。
//! - `Grid::set_size` の同サイズ早期 return: 内容・カーソル・サイズが変わらないこと。

#[test]
fn cell_write_contents_matches_contents() {
    let mut p = vt100::Parser::new(2, 10, 0);
    // ASCII / 全角 (幅 2) / 結合文字付き。
    p.process("aあe\u{301}".as_bytes());
    let s = p.screen();
    for col in 0..10 {
        let cell = s.cell(0, col).unwrap();
        let mut buf = String::from("x");
        cell.write_contents(&mut buf);
        assert_eq!(buf, format!("x{}", cell.contents()), "col {col}");
    }
}

#[test]
fn same_size_set_size_is_a_noop() {
    let mut p = vt100::Parser::new(3, 10, 5);
    p.process(b"line1\r\nline2\r\nline3\r\nline4");
    let before = p.screen().contents();
    let cursor = p.screen().cursor_position();
    let scrolled = p.screen().scrollback();
    p.set_size(3, 10);
    assert_eq!(p.screen().contents(), before);
    assert_eq!(p.screen().cursor_position(), cursor);
    assert_eq!(p.screen().scrollback(), scrolled);
    assert_eq!(p.screen().size(), (3, 10));
    // 実際に変えれば反映される (早期 return が効きすぎていない)。
    p.set_size(4, 12);
    assert_eq!(p.screen().size(), (4, 12));
}
