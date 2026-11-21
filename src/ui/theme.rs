use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone)]
pub struct Theme {
    pub border_idle: Style,
    pub border_focused: Style,
    pub border_claude: Style,
    pub tab_active: Style,
    pub tab_inactive: Style,
    /// 保留 (Alt+C) のタブ。ほかのタブより暗い灰色で「今は見なくていい」を示す。
    /// マーカーの丸もこの色で塗る。
    pub tab_held: Style,
    /// ドラッグで引いている最中の (浮いた) タブ。前面のタブと同じ白帯にする:
    /// オレンジの帯だと状態マーカー 🟨 が背景に溶けて見えない (実機で確認)。
    /// 浮いていることは、ほかのタブの上に重なって動くことで伝わる。
    pub tab_floating: Style,
    /// 名前の入力を始めた直後 (今の名前が丸ごと選ばれている) のタブ。Windows の
    /// 文字選択と同じ青帯に白文字: 前面のタブの白帯と見分けがつき、打てば
    /// 置き換わることが伝わる。1 文字打つと前面のタブの白帯に戻る。
    pub tab_rename_selected: Style,
    pub section_active: Style,
    pub section_inactive: Style,
    pub hint: Style,
    /// ステータスバーのモデル名 (border_claude と同系のオレンジ)。
    pub status_model: Style,
    /// ステータスバーの git ブランチ。
    pub status_branch: Style,
    /// ステータスバーの利用制限 % (最大の枠が 70% 未満)。
    pub status_usage: Style,
    /// 利用制限 % (70% 以上)。
    pub status_usage_warn: Style,
    /// 利用制限 % (90% 以上)。
    pub status_usage_crit: Style,
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
    /// コンテキストメニューのハイライト行 (tab_active と同系の白帯)。
    pub menu_highlight: Style,
    /// コンテキストメニューの無効項目 (グレーアウト)。
    pub menu_disabled: Style,
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
        tab_held: Style::default().fg(Color::DarkGray),
        tab_floating: Style::default()
            .fg(Color::Black)
            .bg(Color::White)
            .add_modifier(Modifier::BOLD),
        tab_rename_selected: Style::default()
            .fg(Color::White)
            .bg(Color::Rgb(0, 120, 215))
            .add_modifier(Modifier::BOLD),
        section_active: Style::default()
            .fg(Color::Black)
            .bg(Color::White)
            .add_modifier(Modifier::BOLD),
        section_inactive: Style::default().fg(Color::DarkGray),
        hint: Style::default().fg(Color::DarkGray),
        status_model: Style::default().fg(Color::Rgb(255, 140, 0)),
        status_branch: Style::default().fg(Color::Rgb(148, 163, 184)),
        status_usage: Style::default().fg(Color::Rgb(125, 211, 252)),
        status_usage_warn: Style::default().fg(Color::Rgb(250, 204, 21)),
        status_usage_crit: Style::default()
            .fg(Color::Rgb(248, 113, 113))
            .add_modifier(Modifier::BOLD),
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
        menu_highlight: Style::default()
            .fg(Color::Black)
            .bg(Color::White)
            .add_modifier(Modifier::BOLD),
        menu_disabled: Style::default().fg(Color::DarkGray),
    }
}

// Version History
// ver0.1 - 2026-04-25 - Added file tree category colors for iconized sidebar rows.
// ver0.2 - 2026-07-12 - Added context-menu styles (menu_highlight / menu_disabled).
// ver0.3 - 2026-07-20 - Added status bar styles (status_model / status_branch).
// ver0.4 - 2026-08-11 - Added Claude pane status colors
//                       (status_busy / status_attention / status_done).
// ver0.5 - 2026-10-06 - Added plan-usage styles for the status bar
//                       (status_usage sky / status_usage_warn yellow /
//                       status_usage_crit bold red).
// ver0.6 - 2026-11-08 - Removed the Claude pane status colours
//                       (status_busy / status_attention / status_done): the
//                       state is shown by the tab marker emoji only.
// ver0.7 - 2026-11-15 - Added tab_floating for the tab being dragged in the
//                       tab bar: the same white band as tab_active (an orange
//                       band hid the 🟨 status marker).
// ver0.8 - 2026-11-21 - Added tab_rename_selected: the tab whose name is being
//                       edited and still fully selected (nothing typed yet) is
//                       drawn as white on a blue band, like selected text.
// ver0.9 - 2026-11-21 - Added tab_held: a tab put on hold with Alt+C is drawn
//                       in dark grey (marker dot and number).
