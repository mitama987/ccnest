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
use crate::sidebar::{claude_ctx, panelist, Section};

pub fn draw(app: &App, frame: &mut Frame<'_>, pane_rects: &mut HashMap<PaneId, AppRect>) {
    let size = frame.area();
    let theme = theme::default_theme();

    let cols = if app.sidebar.visible {
        vec![Constraint::Length(32), Constraint::Min(10)]
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

    if let Some(area) = sidebar_area {
        draw_sidebar(app, frame, area, &theme);
    }

    let vert = LLayout::default()
        .direction(LDir::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(main_area);
    draw_tabbar(app, frame, vert[0], &theme);
    draw_panes(app, frame, vert[1], pane_rects, &theme);
    draw_statusbar(app, frame, vert[2], &theme);
}

fn draw_tabbar(app: &App, frame: &mut Frame<'_>, area: Rect, theme: &theme::Theme) {
    let mut spans = Vec::new();
    for (i, tab) in app.tabs.iter().enumerate() {
        let active = i == app.active_tab;
        let style = if active {
            theme.tab_active
        } else {
            theme.tab_inactive
        };
        spans.push(Span::styled(format!(" {} ", tab.title), style));
        spans.push(Span::raw(" "));
    }
    let hint = Span::styled(
        "  Ctrl+D:━  Ctrl+E:┃  Ctrl+T:tab  Ctrl+W:close  Ctrl+B:sidebar  Ctrl+Q:quit",
        theme.hint,
    );
    spans.push(hint);
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_statusbar(app: &App, frame: &mut Frame<'_>, area: Rect, theme: &theme::Theme) {
    let text = app
        .status
        .clone()
        .unwrap_or_else(|| format!("cwd: {}", app.focused_pane_cwd().display()));
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(text, theme.hint))),
        area,
    );
}

fn draw_sidebar(app: &App, frame: &mut Frame<'_>, area: Rect, theme: &theme::Theme) {
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
        Section::FileTree => app
            .sidebar
            .file_entries
            .iter()
            .map(|e| Line::from(Span::raw(e.display(&app.sidebar.cwd))))
            .collect(),
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
                let widget = PaneCells {
                    parser: &pane.parser,
                };
                frame.render_widget(widget, inner);
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
}

impl<'a> Widget for PaneCells<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let Ok(parser) = self.parser.lock() else {
            return;
        };
        let screen = parser.screen();
        for y in 0..area.height {
            for x in 0..area.width {
                if let Some(cell) = screen.cell(y, x) {
                    let ch = cell.contents();
                    let style = style_from_cell(cell);
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
