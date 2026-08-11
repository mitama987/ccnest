use crate::app::App;
use crate::pane::status::ClaudeStatus;

#[derive(Debug, Clone)]
pub struct PaneRow {
    pub index: usize,
    pub active: bool,
    pub claude: bool,
    pub cwd: String,
    pub command: String,
    /// 「今なにをしているか」(Pane::task のコピー)。フル文字列のまま持ち、
    /// 描画時はサイドバー幅で自然に切れるのに任せる。
    pub task: Option<String>,
    /// 状態色 (タブバーと同じマッピングで描画側が着色する)。
    pub status: ClaudeStatus,
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
            task: pane.task.clone(),
            status: pane.status,
        });
    }
    out
}

impl PaneRow {
    pub fn display(&self) -> String {
        let marker = if self.active { "▶" } else { " " };
        let kind = if self.claude { "⏵" } else { "·" };
        match self.task.as_deref() {
            Some(task) => format!(
                "{marker}[{}] {kind} {} — {task}  ({})",
                self.index, self.command, self.cwd
            ),
            None => format!(
                "{marker}[{}] {kind} {}  ({})",
                self.index, self.command, self.cwd
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(task: Option<&str>) -> PaneRow {
        PaneRow {
            index: 1,
            active: true,
            claude: true,
            cwd: r"C:\work".to_string(),
            command: "claude".to_string(),
            task: task.map(str::to_string),
            status: ClaudeStatus::Idle,
        }
    }

    // タスクがあれば「— タスク」が cwd の前に入る
    #[test]
    fn display_includes_task_before_cwd() {
        assert_eq!(
            row(Some("テスト修正")).display(),
            "▶[1] ⏵ claude — テスト修正  (C:\\work)"
        );
    }

    // タスクが無ければ従来の表示のまま (回帰防止)
    #[test]
    fn display_without_task_is_unchanged() {
        assert_eq!(row(None).display(), "▶[1] ⏵ claude  (C:\\work)");
    }

    // 非アクティブ・shell ペインのマーカーも従来どおり
    #[test]
    fn display_inactive_shell_markers_unchanged() {
        let mut r = row(None);
        r.active = false;
        r.claude = false;
        r.command = "cmd.exe".to_string();
        assert_eq!(r.display(), " [1] · cmd.exe  (C:\\work)");
    }
}

// Version History
// ver0.2 - 2026-08-11 - PaneRow に task / status を追加。タスクがあれば
//                       「— タスク」を挟んで表示し、状態色は描画側で適用。
// ver0.1 - 初版 (index / active / claude / cwd / command のみ)
