//! Regression test for the panic that occurred when ccnest split a pane
//! (Ctrl+D / Ctrl+E) while Claude Code was actively rendering a wide
//! character (e.g. the spinner glyph) near the right edge.
//!
//! The shrink path of `vt100::Parser::set_size` used to leave the first
//! half of a wide character at the new last column without its
//! continuation cell. The next byte that landed on that column made
//! `Screen::text()` follow the `is_wide()` branch and unwrap
//! `drawing_cell_mut(col + 1)` — a `None` — at vt100 screen.rs:977,
//! crashing the PTY reader thread and freezing the UI.
//!
//! With the vendored vt100 patch (`Row::resize` now clears an orphaned
//! wide first-half on shrink), processing input after a width-narrowing
//! resize must not panic.

#[test]
fn shrink_then_write_does_not_panic_on_wide_first_half() {
    // 1 row, 10 cols, no scrollback — simplest layout.
    let mut p = vt100::Parser::new(1, 10, 0);

    // Park the cursor so the wide character lands at col 8 (first half)
    // / col 9 (continuation). Eight ASCII spaces advance the cursor to
    // col 8, then "あ" (width 2) consumes col 8 and col 9. After the
    // wide write the cursor is past the right edge.
    p.process("        あ".as_bytes());

    // Shrink width by one column. The continuation cell at col 9 is now
    // truncated; without the patch the orphaned first half at col 8
    // remains marked is_wide(), and col_clamp leaves the cursor sitting
    // on top of it.
    p.set_size(1, 9);

    // The next byte from the PTY lands at the clamped cursor position
    // (col 8). Screen::text() takes the is_wide() branch and unwraps
    // drawing_cell_mut(col 9) — out of bounds — at vt100 screen.rs:977.
    // Before the fix this panicked.
    p.process(b"X");
}

#[test]
fn shrink_clears_orphan_wide_first_half_in_grid() {
    let mut p = vt100::Parser::new(1, 10, 0);
    p.process("        あ".as_bytes());

    // Sanity: before resize, the cell at col 8 is wide.
    assert!(
        p.screen().cell(0, 8).unwrap().is_wide(),
        "wide character should occupy col 8 before resize"
    );

    p.set_size(1, 9);

    // After resize, the new last column (col 8) must no longer be marked
    // as the first half of a wide character.
    assert!(
        !p.screen().cell(0, 8).unwrap().is_wide(),
        "orphaned wide first half at new last column must be cleared"
    );
}
