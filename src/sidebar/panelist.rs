use crate::app::App;

#[derive(Debug, Clone)]
pub struct PaneRow {
    pub index: usize,
    pub active: bool,
    pub claude: bool,
    pub cwd: String,
    pub command: String,
}

pub fn rows(app: &App) -> Vec<PaneRow> {
    let mut out = Vec::new();
    let focused = app.current_tab().focused;
    for (i, pid) in app.current_tab().layout.leaves().iter().enumerate() {
        let Some(pane) = app.panes.get(pid) else {
            continue;
        };
        out.push(PaneRow {
            index: i + 1,
            active: *pid == focused,
            claude: pane.claude_running,
            cwd: pane.cwd.display().to_string(),
            command: pane.command.clone(),
        });
    }
    out
}

impl PaneRow {
    pub fn display(&self) -> String {
        let marker = if self.active { "▶" } else { " " };
        let kind = if self.claude { "⏵" } else { "·" };
        format!(
            "{marker}[{}] {kind} {}  ({})",
            self.index, self.command, self.cwd
        )
    }
}
