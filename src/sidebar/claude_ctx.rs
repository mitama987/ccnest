use crate::app::App;
use crate::claude::session::{session_path, usage_from_file, ContextUsage};

#[derive(Debug, Clone)]
pub struct PaneCtxRow {
    pub pane_index: usize,
    pub active: bool,
    pub usage: Option<ContextUsage>,
}

pub fn rows(app: &App) -> Vec<PaneCtxRow> {
    let focused = app.current_tab().focused;
    let mut out = Vec::new();
    for (i, pid) in app.current_tab().layout.leaves().iter().enumerate() {
        let Some(pane) = app.panes.get(pid) else {
            continue;
        };
        let usage = session_path(&pane.cwd, &pane.session_id.to_string())
            .and_then(|p| usage_from_file(&p).ok());
        out.push(PaneCtxRow {
            pane_index: i + 1,
            active: *pid == focused,
            usage,
        });
    }
    out
}

impl PaneCtxRow {
    pub fn display(&self) -> String {
        let marker = if self.active { "▶" } else { " " };
        match self.usage {
            Some(u) => {
                let pct = (u.ratio() * 100.0).round() as u32;
                format!(
                    "{marker}[{}] {}/{}k ({pct}%)",
                    self.pane_index,
                    u.used / 1000,
                    u.window / 1000
                )
            }
            None => format!("{marker}[{}] (no session yet)", self.pane_index),
        }
    }
}
