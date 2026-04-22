use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone)]
pub struct Theme {
    pub border_idle: Style,
    pub border_focused: Style,
    pub border_claude: Style,
    pub tab_active: Style,
    pub tab_inactive: Style,
    pub section_active: Style,
    pub section_inactive: Style,
    pub hint: Style,
    /// The headline "white block reversed" cursor for the sidebar selection.
    pub cursor_row: Style,
}

pub fn default_theme() -> Theme {
    Theme {
        border_idle: Style::default().fg(Color::DarkGray),
        border_focused: Style::default().fg(Color::LightCyan),
        border_claude: Style::default().fg(Color::Rgb(255, 140, 0)), // orange, ccmux-compatible
        tab_active: Style::default()
            .fg(Color::Black)
            .bg(Color::White)
            .add_modifier(Modifier::BOLD),
        tab_inactive: Style::default().fg(Color::Gray),
        section_active: Style::default()
            .fg(Color::Black)
            .bg(Color::White)
            .add_modifier(Modifier::BOLD),
        section_inactive: Style::default().fg(Color::DarkGray),
        hint: Style::default().fg(Color::DarkGray),
        cursor_row: Style::default()
            .fg(Color::Black)
            .bg(Color::White)
            .add_modifier(Modifier::REVERSED | Modifier::BOLD),
    }
}
