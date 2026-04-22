use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::ui::theme::Theme;

/// Re-style a line as the "white block reversed" selection row.
/// Implementation note: we explicitly set a white bg + black fg AND set
/// REVERSED, so the row stands out as a solid block regardless of terminal
/// theme.
pub fn highlight<'a>(line: Line<'a>, theme: &Theme) -> Line<'a> {
    let style: Style = theme.cursor_row;
    let spans: Vec<Span<'a>> = line
        .spans
        .into_iter()
        .map(|s| Span::styled(s.content, style))
        .collect();
    Line::from(spans)
}
