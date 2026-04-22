use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    SplitHorizontal,
    SplitVertical,
    NewTab,
    ClosePane,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    NextTab,
    PrevTab,
    ToggleSidebar,
    ToggleFileTree,
    SidebarSection(u8),
    SidebarCursorUp,
    SidebarCursorDown,
    SidebarOpenEntry,
    SidebarCycleSection,
    FocusSidebar,
    FocusContent,
    BeginRenameTab,
    PassThrough,
}

pub fn resolve(ev: &KeyEvent, sidebar_focused: bool) -> Action {
    let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
    let alt = ev.modifiers.contains(KeyModifiers::ALT);
    let shift = ev.modifiers.contains(KeyModifiers::SHIFT);

    // F2 → begin rename (no modifier required).
    if let KeyCode::F(2) = ev.code {
        return Action::BeginRenameTab;
    }

    // Global multiplexer hotkeys (prefix-less) — claimed before pass-through.
    if ctrl {
        match ev.code {
            // Split: D = vertical (right), E = horizontal (down).
            KeyCode::Char('d') | KeyCode::Char('D') => return Action::SplitVertical,
            KeyCode::Char('e') | KeyCode::Char('E') => return Action::SplitHorizontal,
            KeyCode::Char('t') | KeyCode::Char('T') => return Action::NewTab,
            KeyCode::Char('w') | KeyCode::Char('W') => return Action::ClosePane,
            KeyCode::Char('b') | KeyCode::Char('B') => return Action::ToggleSidebar,
            KeyCode::Char('f') | KeyCode::Char('F') => return Action::ToggleFileTree,
            KeyCode::Char('q') | KeyCode::Char('Q') => return Action::Quit,
            KeyCode::Char('1') => return Action::SidebarSection(0),
            KeyCode::Char('2') => return Action::SidebarSection(1),
            KeyCode::Char('3') => return Action::SidebarSection(2),
            KeyCode::Char('4') => return Action::SidebarSection(3),
            // Pane focus on Ctrl+arrow.
            KeyCode::Left => return Action::FocusLeft,
            KeyCode::Right => return Action::FocusRight,
            KeyCode::Up => return Action::FocusUp,
            KeyCode::Down => return Action::FocusDown,
            // Legacy alias for tab switching.
            KeyCode::Tab => {
                return if shift {
                    Action::PrevTab
                } else {
                    Action::NextTab
                };
            }
            _ => {}
        }
    }
    if alt {
        match ev.code {
            // Tab navigation on Alt+left/right.
            KeyCode::Left => return Action::PrevTab,
            KeyCode::Right => return Action::NextTab,
            _ => {}
        }
    }

    // Sidebar-local navigation takes precedence when focus is in sidebar.
    if sidebar_focused {
        match ev.code {
            KeyCode::Up | KeyCode::Char('k') => return Action::SidebarCursorUp,
            KeyCode::Down | KeyCode::Char('j') => return Action::SidebarCursorDown,
            KeyCode::Enter => return Action::SidebarOpenEntry,
            KeyCode::Tab => return Action::SidebarCycleSection,
            KeyCode::Esc => return Action::FocusContent,
            _ => {}
        }
    }

    Action::PassThrough
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn ctrl_d_is_split_vertical() {
        assert_eq!(
            resolve(&key(KeyCode::Char('d'), KeyModifiers::CONTROL), false),
            Action::SplitVertical
        );
    }

    #[test]
    fn ctrl_e_is_split_horizontal() {
        assert_eq!(
            resolve(&key(KeyCode::Char('e'), KeyModifiers::CONTROL), false),
            Action::SplitHorizontal
        );
    }

    #[test]
    fn ctrl_t_is_new_tab() {
        assert_eq!(
            resolve(&key(KeyCode::Char('t'), KeyModifiers::CONTROL), false),
            Action::NewTab
        );
    }

    #[test]
    fn plain_letter_passes_through() {
        assert_eq!(
            resolve(&key(KeyCode::Char('a'), KeyModifiers::NONE), false),
            Action::PassThrough
        );
    }

    #[test]
    fn arrow_in_sidebar_navigates() {
        assert_eq!(
            resolve(&key(KeyCode::Up, KeyModifiers::NONE), true),
            Action::SidebarCursorUp
        );
    }

    #[test]
    fn ctrl_arrow_moves_pane_focus() {
        assert_eq!(
            resolve(&key(KeyCode::Left, KeyModifiers::CONTROL), false),
            Action::FocusLeft
        );
        assert_eq!(
            resolve(&key(KeyCode::Right, KeyModifiers::CONTROL), false),
            Action::FocusRight
        );
        assert_eq!(
            resolve(&key(KeyCode::Up, KeyModifiers::CONTROL), false),
            Action::FocusUp
        );
        assert_eq!(
            resolve(&key(KeyCode::Down, KeyModifiers::CONTROL), false),
            Action::FocusDown
        );
    }

    #[test]
    fn alt_arrow_switches_tabs() {
        assert_eq!(
            resolve(&key(KeyCode::Right, KeyModifiers::ALT), false),
            Action::NextTab
        );
        assert_eq!(
            resolve(&key(KeyCode::Left, KeyModifiers::ALT), false),
            Action::PrevTab
        );
    }

    #[test]
    fn ctrl_f_toggles_file_tree() {
        assert_eq!(
            resolve(&key(KeyCode::Char('f'), KeyModifiers::CONTROL), false),
            Action::ToggleFileTree
        );
    }

    #[test]
    fn f2_begins_rename() {
        assert_eq!(
            resolve(&key(KeyCode::F(2), KeyModifiers::NONE), false),
            Action::BeginRenameTab
        );
    }
}
