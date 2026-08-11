pub mod cursor;
pub mod theme;

use std::collections::HashMap;

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction as LDir, Layout as LLayout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};
use ratatui::Frame;

use crate::app::{App, Rect as AppRect};
use crate::pane::grid::{Layout, SplitDir};
use crate::pane::status::{aggregate_status, ClaudeStatus};
use crate::pane::PaneId;
use crate::sidebar::{claude_ctx, filetree, panelist, Section};

pub fn draw(
    app: &App,
    frame: &mut Frame<'_>,
    pane_rects: &mut HashMap<PaneId, AppRect>,
    sidebar_file_rect: &mut Option<AppRect>,
    tab_rects: &mut Vec<(AppRect, usize)>,
    menu_rect: &mut Option<AppRect>,
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

    // コンテキストメニューは最後に描いて最前面に重ねる。実際に描いた矩形を
    // イベント側へ返し、マウスヒットテストは常に「画面に出ているもの」と
    // 一致させる (sidebar_file_rect と同じパターン)。
    *menu_rect = None;
    if let Some(menu) = &app.context_menu {
        *menu_rect = Some(draw_context_menu(menu, frame, size, &theme));
    }
}

/// クリックセル (ax, ay) を基準に、端末 (term_w × term_h) 内へ必ず収まる
/// メニュー矩形を返す純粋関数。幅は「最大ラベル表示幅 + 左右パディング 2
/// と枠 2」、高さは「項目数 + 枠 2」。基本はクリック直下 (ay+1) に出し、
/// 右端はクランプ、下に入らなければクリックの上へ反転、それも入らない極小
/// 端末では下端寄せする。返り値は常に端末内 (ratatui の範囲外描画を防ぐ)。
fn context_menu_rect(
    ax: u16,
    ay: u16,
    n_items: u16,
    max_label_w: u16,
    term_w: u16,
    term_h: u16,
) -> Rect {
    // アンカーはまず端末内へクランプする。メニュー表示中に端末が縮むと、
    // 保存されたアンカーが新しい端末サイズの外を指し得る。そのまま flip 計算
    // (ay - h) すると端末外の矩形になり ratatui の描画が範囲外パニックする。
    let ax = ax.min(term_w.saturating_sub(1));
    let ay = ay.min(term_h.saturating_sub(1));
    let w = (max_label_w + 4).min(term_w);
    let h = (n_items + 2).min(term_h);
    let x = if ax + w <= term_w {
        ax
    } else {
        term_w.saturating_sub(w)
    };
    let y = if ay + 1 + h <= term_h {
        ay + 1
    } else if ay >= h {
        ay - h
    } else {
        term_h.saturating_sub(h)
    };
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

/// コンテキストメニューを最前面に描き、実際に使った矩形 (枠込み) を返す。
fn draw_context_menu(
    menu: &crate::app::ContextMenu,
    frame: &mut Frame<'_>,
    term: Rect,
    theme: &theme::Theme,
) -> AppRect {
    let max_label_w = menu
        .items
        .iter()
        .map(|i| Span::raw(i.label).width() as u16)
        .max()
        .unwrap_or(0);
    let rect = context_menu_rect(
        menu.anchor.0,
        menu.anchor.1,
        menu.items.len() as u16,
        max_label_w,
        term.width,
        term.height,
    );

    let inner_w = rect.width.saturating_sub(2) as usize;
    let lines: Vec<Line<'_>> = menu
        .items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            // ハイライト帯が行全域に伸びるよう内側幅までスペースで埋める。
            let text = format!(" {}", item.label);
            let pad = inner_w.saturating_sub(Span::raw(text.as_str()).width());
            let padded = format!("{}{}", text, " ".repeat(pad));
            let style = if !item.enabled {
                theme.menu_disabled
            } else if i == menu.highlighted {
                theme.menu_highlight
            } else {
                Style::default()
            };
            Line::from(Span::styled(padded, style))
        })
        .collect();

    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme.border_focused),
        ),
        rect,
    );

    AppRect {
        x: rect.x as i32,
        y: rect.y as i32,
        w: rect.width as i32,
        h: rect.height as i32,
    }
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
        let base = if active {
            theme.tab_active
        } else {
            theme.tab_inactive
        };
        // タブの状態色 = 配下全ペインの畳み込み。タスク名はフォーカスペインのもの。
        let status = aggregate_status(
            tab.layout
                .leaves()
                .iter()
                .filter_map(|pid| app.panes.get(pid))
                .map(|p| p.status),
        );
        let task = app.panes.get(&tab.focused).and_then(|p| p.task.as_deref());
        let style = tab_label_style(base, status, theme);
        let label = if active && app.renaming_tab.is_some() {
            format!(" {}\u{258e} ", app.renaming_tab.as_deref().unwrap_or(""))
        } else {
            format!(" {} ", tab_label(i + 1, &tab.title, task, TAB_LABEL_MAX))
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

/// ステータスバー 1 行目のセグメント区切り (表示幅 3 桁)。
const STATUS_SEP: &str = " │ ";
const STATUS_SEP_W: usize = 3;
/// cwd をこの幅未満までしか残せないなら、いっそ丸ごと落とす。
const MIN_CWD_WIDTH: usize = 10;
/// ブランチ名をこの幅未満までしか残せないなら、丸ごと落とす。
const MIN_BRANCH_WIDTH: usize = 8;

/// 幅計算済みのステータスバー 1 行目。空文字列 = そのセグメントは出さない。
#[derive(Debug, Default, PartialEq, Eq)]
struct StatusLayout {
    cwd: String,
    model: String,
    branch: String,
}

impl StatusLayout {
    fn segments(&self) -> impl Iterator<Item = (&str, u8)> {
        [
            (self.cwd.as_str(), 0u8),
            (self.model.as_str(), 1),
            (self.branch.as_str(), 2),
        ]
        .into_iter()
        .filter(|(s, _)| !s.is_empty())
    }
}

fn width_of(s: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(s)
}

/// 末尾を残して先頭を `…` で詰める (パスは末尾のディレクトリ名が重要)。
fn ellipsize_left(s: &str, max: usize) -> String {
    if width_of(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    if max == 1 {
        return "…".to_string();
    }
    let budget = max - 1;
    let mut tail: Vec<char> = Vec::new();
    let mut w = 0;
    for ch in s.chars().rev() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > budget {
            break;
        }
        w += cw;
        tail.push(ch);
    }
    let mut out = String::from("…");
    out.extend(tail.into_iter().rev());
    out
}

/// 先頭を残して末尾を `…` で詰める。
fn ellipsize_right(s: &str, max: usize) -> String {
    if width_of(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    if max == 1 {
        return "…".to_string();
    }
    let budget = max - 1;
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > budget {
            break;
        }
        w += cw;
        out.push(ch);
    }
    out.push('…');
    out
}

fn joined_width(parts: &[&str]) -> usize {
    let live: Vec<&&str> = parts.iter().filter(|s| !s.is_empty()).collect();
    if live.is_empty() {
        return 0;
    }
    live.iter().map(|s| width_of(s)).sum::<usize>() + STATUS_SEP_W * (live.len() - 1)
}

/// 幅 `width` に収まるようセグメントを削る。
///
/// **モデル名とブランチを最優先で守る**（それが今回追加した情報であり、cwd は
/// 既にタブタイトルにも出ているため）。削る順は cwd → ブランチ → モデル。
fn layout_status(cwd: &str, model: &str, branch: &str, width: usize) -> StatusLayout {
    let mut cwd = cwd.to_string();
    let mut branch = branch.to_string();
    let model = model.to_string();

    if joined_width(&[&cwd, &model, &branch]) <= width {
        return StatusLayout { cwd, model, branch };
    }

    // 1) cwd を左から詰める。確保できる幅が足りなければ丸ごと落とす。
    if !cwd.is_empty() {
        let others = joined_width(&[&model, &branch]);
        let sep = if others > 0 { STATUS_SEP_W } else { 0 };
        let avail = width.saturating_sub(others + sep);
        cwd = if avail >= MIN_CWD_WIDTH {
            ellipsize_left(&cwd, avail)
        } else {
            String::new()
        };
        if joined_width(&[&cwd, &model, &branch]) <= width {
            return StatusLayout { cwd, model, branch };
        }
    }

    // 2) ブランチを右から詰める。
    if !branch.is_empty() {
        let others = joined_width(&[&cwd, &model]);
        let sep = if others > 0 { STATUS_SEP_W } else { 0 };
        let avail = width.saturating_sub(others + sep);
        branch = if avail >= MIN_BRANCH_WIDTH {
            ellipsize_right(&branch, avail)
        } else {
            String::new()
        };
        if joined_width(&[&cwd, &model, &branch]) <= width {
            return StatusLayout { cwd, model, branch };
        }
    }

    // 3) 最後の砦。モデル名だけを幅に押し込む。
    StatusLayout {
        cwd: String::new(),
        model: ellipsize_right(&model, width),
        branch: String::new(),
    }
}

/// ステータスバー 1 行目を組み立てる純粋関数。App に触らないので
/// TestBackend で実バッファを検証できる。
fn status_line<'a>(
    cwd: &str,
    model: &str,
    branch: &str,
    width: usize,
    theme: &theme::Theme,
) -> Line<'a> {
    let layout = layout_status(cwd, model, branch, width);
    let mut spans: Vec<Span> = Vec::new();
    for (text, kind) in layout.segments() {
        if !spans.is_empty() {
            spans.push(Span::styled(STATUS_SEP, theme.hint));
        }
        let style = match kind {
            1 => theme.status_model,
            2 => theme.status_branch,
            _ => theme.hint,
        };
        spans.push(Span::styled(text.to_string(), style));
    }
    Line::from(spans)
}

/// タブラベルの表示幅上限 (セル)。タスク名が長くてもタブが際限なく
/// 育たないよう抑える。
const TAB_LABEL_MAX: usize = 20;

/// タブ 1 個のラベル文字列。タスクがあれば「N:短縮タスク」、無ければ従来の
/// タイトル。どちらも `max` セルに `ellipsize_right` (unicode-width ベース)
/// で収める。
fn tab_label(number: usize, title: &str, task: Option<&str>, max: usize) -> String {
    let raw = match task {
        Some(t) => format!("{number}:{t}"),
        None => title.to_string(),
    };
    ellipsize_right(&raw, max)
}

/// ペイン状態をタブ/サイドバー行のスタイルに反映する。Idle は base のまま。
/// fg だけを差し替え、bg と modifier (アクティブタブの白帯・BOLD) は保持する。
fn tab_label_style(base: Style, status: ClaudeStatus, theme: &theme::Theme) -> Style {
    let status_style = match status {
        ClaudeStatus::Idle => return base,
        ClaudeStatus::Busy => theme.status_busy,
        ClaudeStatus::NeedsAttention => theme.status_attention,
        ClaudeStatus::DoneUnseen => theme.status_done,
    };
    match status_style.fg {
        Some(fg) => base.fg(fg),
        None => base,
    }
}

fn draw_statusbar(app: &App, frame: &mut Frame<'_>, area: Rect, theme: &theme::Theme) {
    let hint_text =
        "Ctrl+D:┃  Ctrl+E:━  Ctrl+T:tab  Ctrl+W:close  Ctrl+F:files  Alt+F:rename  Ctrl+C×2:shell  Ctrl+Q:quit";
    let cwd = app
        .status
        .clone()
        .unwrap_or_else(|| format!("cwd: {}", app.focused_pane_cwd().display()));
    // claude が走っていないペインでもセグメントは消さない (消すと右側が
    // フォーカス移動のたびに伸縮してチラつく)。記号マーカーは付けない —
    // ✳ (U+2733) 等は Windows Terminal が絵文字表示にして 2 桁を占め、
    // unicode-width の申告 (1 桁) と食い違って行がはみ出す。
    let model = app
        .focused_model_label()
        .unwrap_or_else(|| "shell".to_string());
    let branch = app
        .focused_branch()
        .map(|b| format!("⎇ {b}"))
        .unwrap_or_default();

    // 上段: cwd / status + モデル + ブランチ、下段: ショートカットヒント
    let lines = vec![
        status_line(&cwd, &model, &branch, area.width as usize, theme),
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
            .map(|r| {
                // 状態色はタブバーと同じマッピング (Idle はデフォルト表示)。
                let style = tab_label_style(Style::default(), r.status, theme);
                Line::from(Span::styled(r.display(), style))
            })
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
                let selection = app.selection.filter(|s| s.pane_id == *pid).map(|s| {
                    let (start, end) = normalize_selection(s.anchor, s.cursor);
                    SelectionDraw {
                        start,
                        end,
                        grid: (s.alt, s.grid_gen),
                    }
                });
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

/// PaneCells へ渡す選択の描画情報。座標は正規化済みのバッファ絶対座標
/// (start <= end)。描画時に現在の viewport_top_abs を引いて画面座標へ変換する。
/// 変換結果が負 / 画面高以上の行は描画ループの範囲外となり自動的に対象外。
struct SelectionDraw {
    start: (u16, i64),
    end: (u16, i64),
    /// 選択作成時のグリッド識別 (alternate_screen, reset_generation)。
    /// 描画時点の状態と食い違っていたら (validate_selection が破棄する前の
    /// 1 フレーム) 反転を出さない。変換と同一ロック下で判定する。
    grid: (bool, u64),
}

struct PaneCells<'a> {
    parser: &'a std::sync::Mutex<vt100::Parser>,
    selection: Option<SelectionDraw>,
}

impl<'a> Widget for PaneCells<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let Ok(parser) = self.parser.lock() else {
            return;
        };
        let screen = parser.screen();
        // abs -> screen 変換はフレームごとに 1 回。以降は従来どおり画面座標で判定。
        // グリッド識別が食い違う選択 (直前に alt 切替 / RIS が起きた) は描かない。
        let selection = self
            .selection
            .filter(|sel| (screen.alternate_screen(), screen.reset_generation()) == sel.grid)
            .map(|sel| {
                let top = crate::pane::viewport_top_abs(screen);
                (
                    (sel.start.0, sel.start.1 - top),
                    (sel.end.0, sel.end.1 - top),
                )
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
    fn context_menu_rect_opens_below_click() {
        // anchor (10,5), 2 項目, 最大ラベル幅 8 → 12x4 がクリック直下に出る。
        assert_eq!(
            context_menu_rect(10, 5, 2, 8, 80, 24),
            Rect {
                x: 10,
                y: 6,
                width: 12,
                height: 4
            }
        );
    }

    #[test]
    fn context_menu_rect_clamps_right_edge() {
        // 右端に収まらない場合は左へ寄せる (80 - 12 = 68)。
        assert_eq!(context_menu_rect(75, 5, 2, 8, 80, 24).x, 68);
    }

    #[test]
    fn context_menu_rect_flips_above_at_bottom() {
        // 下に入らない場合はクリックの上へ反転 (22 - 4 = 18)。
        assert_eq!(context_menu_rect(10, 22, 2, 8, 80, 24).y, 18);
    }

    #[test]
    fn context_menu_rect_fits_tiny_terminal() {
        let r = context_menu_rect(9, 2, 2, 8, 10, 3);
        assert!(r.x + r.width <= 10, "rect must fit horizontally: {r:?}");
        assert!(r.y + r.height <= 3, "rect must fit vertically: {r:?}");
    }

    #[test]
    fn context_menu_rect_zero_terminal_no_panic() {
        let r = context_menu_rect(0, 0, 2, 8, 0, 0);
        assert_eq!((r.width, r.height), (0, 0));
    }

    #[test]
    fn context_menu_rect_clamps_stale_anchor_after_shrink() {
        // メニュー表示中に端末が 24 行 → 10 行へ縮み、アンカー (ay=20) が
        // 端末外に取り残されたケース。矩形は必ず新しい端末内に収まる。
        let r = context_menu_rect(10, 20, 2, 8, 80, 10);
        assert!(r.x + r.width <= 80, "must fit horizontally: {r:?}");
        assert!(r.y + r.height <= 10, "must fit vertically: {r:?}");
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

    // ---- status bar layout -------------------------------------------------

    /// 実際に描かれる 1 行の表示幅 (セグメント + 区切り) を再現する。
    fn rendered_width(l: &StatusLayout) -> usize {
        let parts: Vec<&str> = l.segments().map(|(s, _)| s).collect();
        if parts.is_empty() {
            return 0;
        }
        parts.iter().map(|s| width_of(s)).sum::<usize>() + STATUS_SEP_W * (parts.len() - 1)
    }

    const CWD: &str = "cwd: C:\\Users\\mitam\\Desktop\\work\\90_other\\ccnest";
    const MODEL: &str = "Opus 4.8";
    const BRANCH: &str = "⎇ feature/threadstoolspro2-note-article";

    #[test]
    fn statusbar_keeps_everything_when_wide() {
        let l = layout_status(CWD, MODEL, BRANCH, 140);
        assert_eq!(l.cwd, CWD);
        assert_eq!(l.model, MODEL);
        assert_eq!(l.branch, BRANCH);
    }

    #[test]
    fn statusbar_truncates_cwd_before_model_and_branch() {
        let l = layout_status(CWD, MODEL, BRANCH, 80);
        // 追加した情報は無傷、cwd だけが削られる。
        assert_eq!(l.model, MODEL);
        assert_eq!(l.branch, BRANCH);
        assert!(l.cwd.starts_with('…'), "cwd は左から詰める: {:?}", l.cwd);
        // 末尾のディレクトリ名は残っている。
        assert!(l.cwd.ends_with("ccnest"), "{:?}", l.cwd);
        assert!(rendered_width(&l) <= 80);
    }

    #[test]
    fn statusbar_drops_cwd_then_truncates_branch() {
        let l = layout_status(CWD, MODEL, BRANCH, 40);
        assert_eq!(l.model, MODEL, "モデル名は最後まで守る");
        assert!(l.cwd.is_empty(), "cwd が先に落ちる: {:?}", l.cwd);
        assert!(
            l.branch.ends_with('…'),
            "ブランチは右から詰める: {:?}",
            l.branch
        );
        assert!(rendered_width(&l) <= 40);
    }

    #[test]
    fn statusbar_model_survives_at_minimum_width() {
        let l = layout_status(CWD, MODEL, BRANCH, 12);
        assert!(l.cwd.is_empty());
        assert!(l.branch.is_empty());
        assert!(!l.model.is_empty(), "モデル名が最後の砦");
        assert!(rendered_width(&l) <= 12);
    }

    #[test]
    fn statusbar_never_exceeds_width_across_the_whole_range() {
        for w in 0..=160usize {
            let l = layout_status(CWD, MODEL, BRANCH, w);
            assert!(
                rendered_width(&l) <= w,
                "width {w} overflowed: {l:?} -> {}",
                rendered_width(&l)
            );
        }
    }

    #[test]
    fn statusbar_handles_multibyte_cwd_width() {
        // 全角は 1 文字 2 桁。バイト長で測っていると必ずはみ出す。
        let cwd = "cwd: C:\\Users\\mitam\\Desktop\\work\\50_ブログ\\記事";
        for w in 0..=120usize {
            let l = layout_status(cwd, MODEL, "⎇ main", w);
            assert!(rendered_width(&l) <= w, "width {w}: {l:?}");
        }
    }

    #[test]
    fn statusbar_without_branch_outside_repo() {
        let l = layout_status(CWD, MODEL, "", 140);
        assert!(l.branch.is_empty());
        assert_eq!(l.cwd, CWD);
        assert_eq!(l.model, MODEL);
    }

    /// 実際に端末バッファへ描いた 1 行目の文字列を取り出す。
    fn render_status_row(cwd: &str, model: &str, branch: &str, width: u16) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let theme = theme::default_theme();
        let mut term = Terminal::new(TestBackend::new(width, 1)).unwrap();
        term.draw(|f| {
            let line = status_line(cwd, model, branch, width as usize, &theme);
            f.render_widget(Paragraph::new(vec![line]), f.area());
        })
        .unwrap();
        let buf = term.backend().buffer();
        (0..width)
            .map(|x| buf[(x, 0)].symbol())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn statusbar_renders_expected_row_at_realistic_width() {
        let row = render_status_row(
            "cwd: C:\\Users\\mitam\\Desktop\\work\\90_other\\ccnest",
            "Opus 4.8",
            "⎇ main",
            100,
        );
        assert_eq!(
            row,
            "cwd: C:\\Users\\mitam\\Desktop\\work\\90_other\\ccnest │ Opus 4.8 │ ⎇ main"
        );
    }

    #[test]
    fn statusbar_renders_shell_pane_placeholder() {
        let row = render_status_row("cwd: C:\\tmp", "shell", "⎇ main", 60);
        assert_eq!(row, "cwd: C:\\tmp │ shell │ ⎇ main");
    }

    #[test]
    fn statusbar_render_never_wraps_to_a_second_cell_row() {
        // 描画バッファ幅を超えないこと = 2 行目 (ヒント行) を押し出さない。
        for w in [20u16, 40, 60, 80, 100, 120] {
            let row = render_status_row(CWD, MODEL, BRANCH, w);
            assert!(width_of(&row) <= w as usize, "w={w}: {row:?}");
        }
    }

    // ---- tab label ---------------------------------------------------------

    // タスクがあれば「N:タスク」形式になる
    #[test]
    fn tab_label_uses_task_when_present() {
        assert_eq!(
            tab_label(1, "ccnest", Some("Fix the tab bar"), 20),
            "1:Fix the tab bar"
        );
    }

    // タスクが無ければ従来どおりタイトル
    #[test]
    fn tab_label_falls_back_to_title() {
        assert_eq!(tab_label(2, "ccnest", None, 20), "ccnest");
    }

    // 長いタスクは … で切り詰められ、上限幅を超えない
    #[test]
    fn tab_label_truncates_with_ellipsis() {
        let l = tab_label(1, "t", Some("a very long task description here"), 20);
        assert!(l.ends_with('…'), "{l:?}");
        assert!(width_of(&l) <= 20, "{l:?}");
    }

    // 全幅ループ不変条件: どの幅でもはみ出さない (全角タスク含む)
    #[test]
    fn tab_label_never_exceeds_width_across_the_whole_range() {
        for w in 0..=40usize {
            for task in [
                Some("タブバーの実装をしてください"),
                Some("fix tests"),
                None,
            ] {
                let l = tab_label(3, "タイトル", task, w);
                assert!(width_of(&l) <= w, "w={w} task={task:?}: {l:?}");
            }
        }
    }

    // Idle は base スタイルそのまま
    #[test]
    fn tab_label_style_idle_keeps_base() {
        let theme = theme::default_theme();
        let base = theme.tab_active;
        assert_eq!(tab_label_style(base, ClaudeStatus::Idle, &theme), base);
    }

    // Busy/NeedsAttention/DoneUnseen は fg だけ差し替え、bg と BOLD は保持
    #[test]
    fn tab_label_style_overrides_fg_only() {
        let theme = theme::default_theme();
        let base = theme.tab_active; // 黒文字・白帯・BOLD
        for (status, expect) in [
            (ClaudeStatus::Busy, theme.status_busy.fg),
            (ClaudeStatus::NeedsAttention, theme.status_attention.fg),
            (ClaudeStatus::DoneUnseen, theme.status_done.fg),
        ] {
            let s = tab_label_style(base, status, &theme);
            assert_eq!(s.fg, expect, "{status:?}");
            assert_eq!(s.bg, base.bg, "{status:?} で bg を壊してはいけない");
            assert_eq!(
                s.add_modifier, base.add_modifier,
                "{status:?} で modifier を壊してはいけない"
            );
        }
    }

    /// タブバー 1 行を TestBackend に描いた結果の文字列を取り出す。
    fn render_tab_row(labels: &[(&str, Style)], width: u16) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut term = Terminal::new(TestBackend::new(width, 1)).unwrap();
        term.draw(|f| {
            let mut spans = Vec::new();
            for (text, style) in labels {
                spans.push(Span::styled(format!(" {text} "), *style));
                spans.push(Span::raw(" "));
            }
            f.render_widget(Paragraph::new(Line::from(spans)), f.area());
        })
        .unwrap();
        let buf = term.backend().buffer();
        (0..width)
            .map(|x| buf[(x, 0)].symbol())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    // タスク付きラベルが実バッファに期待どおり並ぶ
    // (全角はTestBackendの継続セルが空白になるため、ここではASCIIで検証。
    //  全角の幅不変条件は tab_label_never_exceeds_width... 側で担保)
    #[test]
    fn tabbar_renders_task_labels_in_buffer() {
        let theme = theme::default_theme();
        let l1 = tab_label(1, "ccnest", Some("fix tests"), 20);
        let l2 = tab_label(2, "ccnest", None, 20);
        let row = render_tab_row(&[(&l1, theme.tab_active), (&l2, theme.tab_inactive)], 40);
        assert_eq!(row, " 1:fix tests   ccnest");
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
// ver0.4 - 2026-07-20 - Status bar line 1 now shows "cwd │ model │ ⎇ branch" with
//                       explicit display-width truncation (cwd shrinks first, the
//                       model name is protected last). Segment assembly lives in
//                       the pure `status_line` so it can be asserted against a
//                       real TestBackend buffer.
// ver0.5 - 2026-08-11 - タブラベルを「N:タスク名」(20 セル上限・ellipsize_right)
//                       に変更し、Claude 状態色 (busy=緑 / attention=黄 /
//                       done-unseen=マゼンタ) を fg 差し替えで適用。サイドバーの
//                       Panes 行にも同じ状態色を反映。
