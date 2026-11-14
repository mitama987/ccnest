use crate::app::App;
use crate::pane::status::{status_marker, ClaudeStatus};

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
    /// 状態。Claude ペインなら行頭にタブバーと同じマーカー (🟢/🔵/🟨) が付く。
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
        // 状態マーカー (🟢 実行中 / 🔵 裏で実行中 / 🟨 対応が必要) は Claude ペインだけ。
        // shell ペイン (Idle) は従来どおり何も付けない。
        let status = if self.claude && self.status != ClaudeStatus::Idle {
            format!("{} ", status_marker(self.status))
        } else {
            String::new()
        };
        let marker = if self.active { "▶" } else { " " };
        let kind = if self.claude { "⏵" } else { "·" };
        match self.task.as_deref() {
            Some(task) => format!(
                "{status}{marker}[{}] {kind} {} — {task}  ({})",
                self.index, self.command, self.cwd
            ),
            None => format!(
                "{status}{marker}[{}] {kind} {}  ({})",
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

    // Claude ペインは状態マーカーが行頭に付く (タブバーと同じ 🟢 / 🔵 / 🟨)
    #[test]
    fn display_prefixes_status_marker_for_claude_pane() {
        let mut r = row(Some("テスト修正"));
        r.status = ClaudeStatus::Busy;
        assert_eq!(r.display(), "🟢 ▶[1] ⏵ claude — テスト修正  (C:\\work)");
        r.status = ClaudeStatus::NeedsAttention;
        assert_eq!(r.display(), "🟨 ▶[1] ⏵ claude — テスト修正  (C:\\work)");
        r.status = ClaudeStatus::Background;
        assert_eq!(r.display(), "🔵 ▶[1] ⏵ claude — テスト修正  (C:\\work)");
    }

    // shell ペインは状態が何であれマーカー無し
    #[test]
    fn display_shell_pane_has_no_status_marker() {
        let mut r = row(None);
        r.claude = false;
        r.status = ClaudeStatus::NeedsAttention;
        assert_eq!(r.display(), "▶[1] · claude  (C:\\work)");
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
// ver0.5 - 2026-11-14 - 裏で実行中 (ClaudeStatus::Background) の行頭に 🔵 (status_marker に追従)
// ver0.4 - 2026-11-09 - 対応が必要のマーカーを 🟧 → 🟨 に (status_marker に追従)
// ver0.3 - 2026-11-08 - display() が Claude ペインの行頭に状態マーカー (🟢/🟧) を
//                       付ける。文字色での状態表示は廃止 (ui 側から削除)。
// ver0.2 - 2026-08-11 - PaneRow に task / status を追加。タスクがあれば
//                       「— タスク」を挟んで表示し、状態色は描画側で適用。
// ver0.1 - 初版 (index / active / claude / cwd / command のみ)
