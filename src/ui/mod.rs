pub mod cursor;
pub mod theme;

use std::collections::HashMap;

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction as LDir, Layout as LLayout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};
use ratatui::Frame;

use crate::app::{App, Rect as AppRect};
use crate::pane::grid::{Layout, SplitDir};
use crate::pane::PaneId;
use crate::sidebar::{claude_ctx, filetree, panelist, Section};

pub fn draw(
    app: &App,
    frame: &mut Frame<'_>,
    pane_rects: &mut HashMap<PaneId, AppRect>,
    sidebar_file_rect: &mut Option<AppRect>,
    tab_rects: &mut Vec<(AppRect, usize)>,
) {
    let size = frame.area();
    let theme = theme::default_theme();

    let cols = if app.sidebar.visible {
        vec![Constraint::Length(21), Constraint::Min(10)]
    } else {
        vec![Constraint::Min(10)]
    };
    let chunks = LLayout::default()
        .direction(LDir::Horizontal)
        .constraints(cols)
        .split(size);

    let (sidebar_area, main_area) = if app.sidebar.visible {
        (Some(chunks[0]), chunks[1])
    } else {
        (None, chunks[0])
    };

    *sidebar_file_rect = None;
    if let Some(area) = sidebar_area {
        draw_sidebar(app, frame, area, &theme, sidebar_file_rect);
    }

    let vert = LLayout::default()
        .direction(LDir::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(main_area);
    draw_tabbar(app, frame, vert[0], &theme, tab_rects);
    draw_panes(app, frame, vert[1], pane_rects, &theme);
    draw_statusbar(app, frame, vert[2], &theme);
}

fn draw_tabbar(
    app: &App,
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &theme::Theme,
    tab_rects: &mut Vec<(AppRect, usize)>,
) {
    tab_rects.clear();
    let mut spans = Vec::new();
    let mut x_offset: u16 = 0;
    let area_end = area.x.saturating_add(area.width);
    for (i, tab) in app.tabs.iter().enumerate() {
        let active = i == app.active_tab;
        let style = if active {
            theme.tab_active
        } else {
            theme.tab_inactive
        };
        let label = if active && app.renaming_tab.is_some() {
            format!(" {}\u{258e} ", app.renaming_tab.as_deref().unwrap_or(""))
        } else {
            format!(" {} ", tab.title)
        };
        let span = Span::styled(label, style);
        let width = span.width() as u16;
        let abs_x = area.x.saturating_add(x_offset);
        if abs_x < area_end {
            let visible_w = area_end.saturating_sub(abs_x).min(width);
            tab_rects.push((
                AppRect {
                    x: abs_x as i32,
                    y: area.y as i32,
                    w: visible_w as i32,
                    h: 1,
                },
                i,
            ));
        }
        spans.push(span);
        spans.push(Span::raw(" "));
        x_offset = x_offset.saturating_add(width).saturating_add(1);
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_statusbar(app: &App, frame: &mut Frame<'_>, area: Rect, theme: &theme::Theme) {
    let hint_text =
        "Ctrl+D:┃  Ctrl+E:━  Ctrl+T:tab  Ctrl+W:close  Ctrl+F:files  Alt+F:rename  Ctrl+C×2:shell  Ctrl+Q:quit";
    let status = app
        .status
        .clone()
        .unwrap_or_else(|| format!("cwd: {}", app.focused_pane_cwd().display()));
    // 上段: cwd / status、下段: ショートカットヒント
    let lines = vec![
        Line::from(Span::styled(status, theme.hint)),
        Line::from(Span::styled(hint_text, theme.hint)),
    ];
    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_sidebar(
    app: &App,
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &theme::Theme,
    sidebar_file_rect: &mut Option<AppRect>,
) {
    let border_style = if app.sidebar_focused {
        theme.border_focused
    } else {
        theme.border_idle
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" ccnest ")
        .border_style(border_style);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let v = LLayout::default()
        .direction(LDir::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(inner);

    // Section tabs line
    let mut spans = Vec::new();
    for sec in Section::all() {
        let style = if sec == app.sidebar.active {
            theme.section_active
        } else {
            theme.section_inactive
        };
        spans.push(Span::styled(
            format!(" {} ({}) ", sec.title(), sec as u8 + 1),
            style,
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), v[0]);

    // Body
    let lines: Vec<Line> = match app.sidebar.active {
        Section::FileTree => {
            // クリック→行→ノード対応のため、Files 描画域 (v[1]) を保存。
            *sidebar_file_rect = Some(AppRect {
                x: v[1].x as i32,
                y: v[1].y as i32,
                w: v[1].width as i32,
                h: v[1].height as i32,
            });
            app.sidebar
                .file_tree
                .flatten()
                .into_iter()
                .map(|(depth, node)| file_tree_row(node, depth, theme))
                .collect()
        }
        Section::Claude => claude_ctx::rows(app)
            .into_iter()
            .map(|r| Line::from(Span::raw(r.display())))
            .collect(),
        Section::Git => match app.sidebar.git_info.as_ref() {
            Some(gi) => vec![Line::from(Span::raw(gi.summary_line()))],
            None => vec![Line::from(Span::styled("(not a git repo)", theme.hint))],
        },
        Section::Panes => panelist::rows(app)
            .into_iter()
            .map(|r| Line::from(Span::raw(r.display())))
            .collect(),
    };

    let cursor_idx = app.sidebar.cursor();
    let body: Vec<Line> = lines
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            if i == cursor_idx && app.sidebar_focused {
                cursor::highlight(line, theme)
            } else {
                line
            }
        })
        .collect();

    frame.render_widget(Paragraph::new(body), v[1]);
}

fn file_tree_row<'a>(node: &filetree::FileNode, depth: usize, theme: &theme::Theme) -> Line<'a> {
    let style = file_entry_style(node.kind, theme);
    let indent = "  ".repeat(depth);
    let chevron = if node.is_dir {
        if node.expanded {
            "▾"
        } else {
            "▸"
        }
    } else {
        " "
    };
    let name = if node.is_dir {
        format!("{}/", node.name)
    } else {
        node.name.clone()
    };
    Line::from(vec![
        Span::raw(indent),
        Span::styled(chevron.to_string(), theme.hint),
        Span::raw(" "),
        Span::styled(filetree::icon_for_kind(node.kind), style),
        Span::raw(" "),
        Span::styled(name, style),
    ])
}

fn file_entry_style(kind: filetree::EntryKind, theme: &theme::Theme) -> Style {
    match kind {
        filetree::EntryKind::Directory => theme.file_directory,
        filetree::EntryKind::Git => theme.file_git,
        filetree::EntryKind::Markdown => theme.file_markdown,
        filetree::EntryKind::Image => theme.file_image,
        filetree::EntryKind::Rust => theme.file_rust,
        filetree::EntryKind::Python => theme.file_python,
        filetree::EntryKind::JavaScript => theme.file_javascript,
        filetree::EntryKind::TypeScript => theme.file_typescript,
        filetree::EntryKind::Web => theme.file_web,
        filetree::EntryKind::Json => theme.file_json,
        filetree::EntryKind::Config => theme.file_config,
        filetree::EntryKind::Shell => theme.file_shell,
        filetree::EntryKind::Lock => theme.file_lock,
        filetree::EntryKind::Dotfile => theme.file_dotfile,
        filetree::EntryKind::Text => theme.file_text,
        filetree::EntryKind::Other => theme.file_other,
    }
}

fn draw_panes(
    app: &App,
    frame: &mut Frame<'_>,
    area: Rect,
    pane_rects: &mut HashMap<PaneId, AppRect>,
    theme: &theme::Theme,
) {
    pane_rects.clear();
    let tab = app.current_tab();
    render_layout(
        app,
        &tab.layout,
        tab.focused,
        area,
        frame,
        pane_rects,
        theme,
    );
}

fn render_layout(
    app: &App,
    layout: &Layout,
    focused: PaneId,
    area: Rect,
    frame: &mut Frame<'_>,
    pane_rects: &mut HashMap<PaneId, AppRect>,
    theme: &theme::Theme,
) {
    match layout {
        Layout::Leaf(pid) => {
            let is_focus = *pid == focused;
            let claude = app
                .panes
                .get(pid)
                .map(|p| p.claude_running)
                .unwrap_or(false);
            let border_style = if is_focus && claude {
                theme.border_claude
            } else if is_focus {
                theme.border_focused
            } else {
                theme.border_idle
            };
            let title = app
                .panes
                .get(pid)
                .map(|p| format!(" [{}] {} ", pid, p.command))
                .unwrap_or_else(|| format!(" [{pid}] (gone) "));
            let block = Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(border_style);
            let inner = block.inner(area);
            frame.render_widget(block, area);

            pane_rects.insert(
                *pid,
                AppRect {
                    x: inner.x as i32,
                    y: inner.y as i32,
                    w: inner.width as i32,
                    h: inner.height as i32,
                },
            );

            if let Some(pane) = app.panes.get(pid) {
                let selection = app
                    .selection
                    .filter(|s| s.pane_id == *pid)
                    .map(|s| normalize_selection(s.anchor, s.cursor));
                let widget = PaneCells {
                    parser: &pane.parser,
                    selection,
                };
                frame.render_widget(widget, inner);

                // フォーカス中ペインのみ、子 vt100 のカーソル位置に実 (ハード
                // ウェア) カーソルを置く。ccnest は従来カーソルを一切描いて
                // おらず、子 (Claude Code 等) が自前で反転ブロックを描いていた
                // 時代はそれで見えていた。だが子が本物のカーソル (DECTCEM 表示)
                // に依存すると、画面上にカーソルが出ず、左右キーで移動しても
                // 見えない / 形が以前と変わったように見える。
                // 非表示 (DECTCEM off) / スクロールバック表示中 / 範囲外では
                // 出さない。サイドバーにフォーカスがあるときも出さない。
                if is_focus && !app.sidebar_focused {
                    if let Ok(parser) = pane.parser.lock() {
                        let screen = parser.screen();
                        if let Some((cx, cy)) = cursor_draw_pos(
                            inner,
                            screen.cursor_position(),
                            screen.hide_cursor(),
                            screen.scrollback(),
                        ) {
                            frame.set_cursor_position((cx, cy));
                        }
                    }
                }

                // Ensure pty is sized to the rendering area.
                pane.resize(inner.height.max(1), inner.width.max(1));
            }
        }
        Layout::Split { dir, ratio, a, b } => {
            let (dir_l, a_size, b_size) = match dir {
                SplitDir::Vertical => {
                    let total = area.width;
                    let a = (total as f32 * ratio).round() as u16;
                    (LDir::Horizontal, a.max(3), total.saturating_sub(a).max(3))
                }
                SplitDir::Horizontal => {
                    let total = area.height;
                    let a = (total as f32 * ratio).round() as u16;
                    (LDir::Vertical, a.max(3), total.saturating_sub(a).max(3))
                }
            };
            let chunks = LLayout::default()
                .direction(dir_l)
                .constraints([Constraint::Length(a_size), Constraint::Length(b_size)])
                .split(area);
            render_layout(app, a, focused, chunks[0], frame, pane_rects, theme);
            render_layout(app, b, focused, chunks[1], frame, pane_rects, theme);
        }
    }
}

struct PaneCells<'a> {
    parser: &'a std::sync::Mutex<vt100::Parser>,
    /// (start, end) のバッファ絶対座標 (col, abs row)。start<=end で正規化済み。
    /// 描画時に現在の viewport_top_abs を引いて画面座標へ変換する。変換結果が
    /// 負 / 画面高以上の行は描画ループの範囲外となり自動的に対象外。
    selection: Option<((u16, i64), (u16, i64))>,
}

impl<'a> Widget for PaneCells<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let Ok(parser) = self.parser.lock() else {
            return;
        };
        let screen = parser.screen();
        // abs -> screen 変換はフレームごとに 1 回。以降は従来どおり画面座標で判定。
        let selection = self.selection.map(|(s, e)| {
            let top = crate::pane::viewport_top_abs(screen);
            ((s.0, s.1 - top), (e.0, e.1 - top))
        });
        for y in 0..area.height {
            for x in 0..area.width {
                if let Some(cell) = screen.cell(y, x) {
                    let ch = cell.contents();
                    let mut style = style_from_cell(cell);
                    if let Some((start, end)) = selection {
                        if selection_contains((x as i64, y as i64), start, end) {
                            style = style.add_modifier(Modifier::REVERSED);
                        }
                    }
                    let bx = area.x + x;
                    let by = area.y + y;
                    let bcell = &mut buf[(bx, by)];
                    if ch.is_empty() {
                        bcell.set_symbol(" ");
                    } else {
                        bcell.set_symbol(&ch);
                    }
                    bcell.set_style(style);
                }
            }
        }
    }
}

fn selection_contains(pos: (i64, i64), start: (u16, i64), end: (u16, i64)) -> bool {
    let (x, y) = pos;
    if y < start.1 || y > end.1 {
        return false;
    }
    if start.1 == end.1 {
        return x >= start.0 as i64 && x <= end.0 as i64;
    }
    if y == start.1 {
        return x >= start.0 as i64;
    }
    if y == end.1 {
        return x <= end.0 as i64;
    }
    true
}

/// フォーカス中ペインの子 vt100 カーソル状態と描画領域 `inner` から、ratatui に
/// 設定すべき実カーソルの絶対座標 `(x, y)` を返す純粋関数 (IO なし=単体テスト可能)。
///
/// 非表示 (`hidden` = DECTCEM off) / スクロールバック表示中 (`scrollback > 0`、
/// live カーソルが可視領域外を指すため) / 領域外のときは `None` (= カーソルを
/// 出さない)。`cursor` は vt100 の `(row, col)` (0 始まり)。vt100 parser は
/// `pane.resize` で `inner` と同じサイズへ揃えられるため、通常 row < height /
/// col < width に収まるが、リサイズ直後の 1 フレームずれに備えて範囲チェックする。
fn cursor_draw_pos(
    inner: Rect,
    cursor: (u16, u16),
    hidden: bool,
    scrollback: usize,
) -> Option<(u16, u16)> {
    if hidden || scrollback > 0 {
        return None;
    }
    let (crow, ccol) = cursor;
    if crow >= inner.height || ccol >= inner.width {
        return None;
    }
    Some((inner.x + ccol, inner.y + crow))
}

/// (anchor, cursor) を行優先で昇順に並べ替える。event.rs 側と同一ルール。
pub fn normalize_selection(a: (u16, i64), b: (u16, i64)) -> ((u16, i64), (u16, i64)) {
    if a.1 < b.1 || (a.1 == b.1 && a.0 <= b.0) {
        (a, b)
    } else {
        (b, a)
    }
}

fn style_from_cell(cell: &vt100::Cell) -> Style {
    let mut s = Style::default();
    s = s.fg(color_from(cell.fgcolor()));
    s = s.bg(color_from(cell.bgcolor()));
    let mut m = Modifier::empty();
    if cell.bold() {
        m |= Modifier::BOLD;
    }
    if cell.italic() {
        m |= Modifier::ITALIC;
    }
    if cell.underline() {
        m |= Modifier::UNDERLINED;
    }
    if cell.inverse() {
        m |= Modifier::REVERSED;
    }
    s.add_modifier(m)
}

fn color_from(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    fn inner() -> Rect {
        Rect {
            x: 5,
            y: 2,
            width: 80,
            height: 24,
        }
    }

    #[test]
    fn cursor_draw_pos_maps_to_absolute() {
        // vt100 (row=3, col=4) → 絶対 (x = inner.x + col = 9, y = inner.y + row = 5)。
        assert_eq!(cursor_draw_pos(inner(), (3, 4), false, 0), Some((9, 5)));
    }

    #[test]
    fn cursor_draw_pos_origin() {
        assert_eq!(cursor_draw_pos(inner(), (0, 0), false, 0), Some((5, 2)));
    }

    #[test]
    fn cursor_draw_pos_hidden_is_none() {
        // DECTCEM off (子がカーソルを隠している) → 出さない。
        assert_eq!(cursor_draw_pos(inner(), (3, 4), true, 0), None);
    }

    #[test]
    fn cursor_draw_pos_scrollback_is_none() {
        // スクロールバック表示中は live カーソルが可視外 → 出さない。
        assert_eq!(cursor_draw_pos(inner(), (3, 4), false, 7), None);
    }

    #[test]
    fn cursor_draw_pos_out_of_bounds_is_none() {
        // 行が領域高さ以上 / 列が領域幅以上 → 出さない (リサイズ直後の保険)。
        assert_eq!(cursor_draw_pos(inner(), (24, 0), false, 0), None);
        assert_eq!(cursor_draw_pos(inner(), (0, 80), false, 0), None);
    }

    #[test]
    fn cursor_draw_pos_bottom_right_corner() {
        // 領域内最右下 (row=23, col=79) は有効。
        assert_eq!(cursor_draw_pos(inner(), (23, 79), false, 0), Some((84, 25)));
    }

    #[test]
    fn selection_contains_single_row_span() {
        let (s, e) = ((3u16, 5i64), (7u16, 5i64));
        assert!(selection_contains((3, 5), s, e));
        assert!(selection_contains((7, 5), s, e));
        assert!(!selection_contains((2, 5), s, e));
        assert!(!selection_contains((8, 5), s, e));
        assert!(!selection_contains((5, 4), s, e));
    }

    #[test]
    fn selection_contains_multi_row_rules() {
        // 先頭行は start.0 以降、末尾行は end.0 以前、中間行は全域。
        let (s, e) = ((10u16, 2i64), (4u16, 6i64));
        assert!(!selection_contains((9, 2), s, e));
        assert!(selection_contains((10, 2), s, e));
        assert!(selection_contains((0, 4), s, e));
        assert!(selection_contains((79, 4), s, e));
        assert!(selection_contains((4, 6), s, e));
        assert!(!selection_contains((5, 6), s, e));
    }

    #[test]
    fn selection_contains_rejects_out_of_range_rows() {
        // abs→screen 変換後に負 (可視より上) / 画面高以上 (可視より下) となった
        // 選択行は、描画ループの y (0..h) と一致しないため自動的に対象外。
        let (s, e) = ((0u16, -5i64), (10u16, -2i64));
        for y in 0..24i64 {
            assert!(!selection_contains((5, y), s, e));
        }
    }
}

// Version History
// ver0.1 - 2026-04-25 - Rendered file tree entries with type-specific icons and colors.
// ver0.2 - 2026-06-06 - Render the hardware cursor at the focused pane's child
//                       vt100 cursor position (respecting DECTCEM hide + scrollback),
//                       so apps relying on the real terminal cursor (e.g. Claude
//                       Code) show a visible, correctly-shaped, moving cursor.
// ver0.3 - 2026-07-12 - Selection rows are buffer-absolute; convert to screen rows
//                       once per frame in PaneCells::render so the highlight stays
//                       glued to content across scrolling and streaming output.
