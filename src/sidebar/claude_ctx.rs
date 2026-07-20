use crate::app::App;
use crate::claude::session::{ContextUsage, SessionState};

#[derive(Debug, Clone)]
pub struct PaneCtxRow {
    pub pane_index: usize,
    pub active: bool,
    pub usage: Option<ContextUsage>,
    pub state: SessionState,
}

/// 描画パスから呼ばれる。**ここでディスクを触ってはいけない** —
/// 実データは `Pane::session` に、イベントループの定期 tick で
/// 読み込み済みのものを参照するだけ。
pub fn rows(app: &App) -> Vec<PaneCtxRow> {
    let focused = app.current_tab().focused;
    let mut out = Vec::new();
    for (i, pid) in app.current_tab().layout.leaves().iter().enumerate() {
        let Some(pane) = app.panes.get(pid) else {
            continue;
        };
        out.push(PaneCtxRow {
            pane_index: i + 1,
            active: *pid == focused,
            usage: pane.session.info().usage,
            state: pane.session.state(),
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
            // かつては読み取り失敗を丸ごと「セッション未開始」に見せていたため、
            // パス生成の恒久バグが誰にも気づかれなかった。両者を区別する。
            None => match self.state {
                SessionState::ReadError => format!("{marker}[{}] (read error)", self.pane_index),
                _ => format!("{marker}[{}] (no session yet)", self.pane_index),
            },
        }
    }
}
