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
    pub file_directory: Style,
    pub file_git: Style,
    pub file_markdown: Style,
    pub file_image: Style,
    pub file_rust: Style,
    pub file_python: Style,
    pub file_javascript: Style,
    pub file_typescript: Style,
    pub file_web: Style,
    pub file_json: Style,
    pub file_config: Style,
    pub file_shell: Style,
    pub file_lock: Style,
    pub file_dotfile: Style,
    pub file_text: Style,
    pub file_other: Style,
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
        file_directory: Style::default()
            .fg(Color::Rgb(255, 204, 64))
            .add_modifier(Modifier::BOLD),
        file_git: Style::default()
            .fg(Color::Rgb(255, 95, 72))
            .add_modifier(Modifier::BOLD),
        file_markdown: Style::default().fg(Color::Rgb(96, 165, 250)),
        file_image: Style::default().fg(Color::Rgb(232, 121, 249)),
        file_rust: Style::default().fg(Color::Rgb(244, 114, 64)),
        file_python: Style::default().fg(Color::Rgb(88, 166, 255)),
        file_javascript: Style::default().fg(Color::Rgb(250, 204, 21)),
        file_typescript: Style::default().fg(Color::Rgb(56, 189, 248)),
        file_web: Style::default().fg(Color::Rgb(45, 212, 191)),
        file_json: Style::default().fg(Color::Rgb(167, 139, 250)),
        file_config: Style::default().fg(Color::Rgb(250, 230, 128)),
        file_shell: Style::default().fg(Color::Rgb(74, 222, 128)),
        file_lock: Style::default().fg(Color::Rgb(148, 163, 184)),
        file_dotfile: Style::default().fg(Color::DarkGray),
        file_text: Style::default().fg(Color::Gray),
        file_other: Style::default().fg(Color::Rgb(96, 165, 250)),
        cursor_row: Style::default()
            .fg(Color::Black)
            .bg(Color::White)
            .add_modifier(Modifier::REVERSED | Modifier::BOLD),
    }
}

// Version History
// ver0.1 - 2026-04-25 - Added file tree category colors for iconized sidebar rows.
