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
    SidebarSection(u8),
    SidebarCursorUp,
    SidebarCursorDown,
    SidebarOpenEntry,
    SidebarCycleSection,
    FocusSidebar,
    FocusContent,
    PassThrough,
}

pub fn resolve(ev: &KeyEvent, sidebar_focused: bool) -> Action {
    let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
    let alt = ev.modifiers.contains(KeyModifiers::ALT);
    let shift = ev.modifiers.contains(KeyModifiers::SHIFT);

    // Global multiplexer hotkeys (prefix-less) — claimed before pass-through.
    if ctrl {
        match ev.code {
            KeyCode::Char('d') | KeyCode::Char('D') => return Action::SplitHorizontal,
            KeyCode::Char('e') | KeyCode::Char('E') => return Action::SplitVertical,
            KeyCode::Char('t') | KeyCode::Char('T') => return Action::NewTab,
            KeyCode::Char('w') | KeyCode::Char('W') => return Action::ClosePane,
            KeyCode::Char('b') | KeyCode::Char('B') => return Action::ToggleSidebar,
            KeyCode::Char('q') | KeyCode::Char('Q') => return Action::Quit,
            KeyCode::Char('1') => return Action::SidebarSection(0),
            KeyCode::Char('2') => return Action::SidebarSection(1),
            KeyCode::Char('3') => return Action::SidebarSection(2),
            KeyCode::Char('4') => return Action::SidebarSection(3),
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
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('H') => return Action::FocusLeft,
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('L') => return Action::FocusRight,
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('K') => return Action::FocusUp,
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('J') => return Action::FocusDown,
            KeyCode::Char('s') | KeyCode::Char('S') => return Action::FocusSidebar,
            KeyCode::Char('c') | KeyCode::Char('C') => return Action::FocusContent,
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
    fn ctrl_d_is_split_horizontal() {
        assert_eq!(
            resolve(&key(KeyCode::Char('d'), KeyModifiers::CONTROL), false),
            Action::SplitHorizontal
        );
    }

    #[test]
    fn ctrl_e_is_split_vertical() {
        assert_eq!(
            resolve(&key(KeyCode::Char('e'), KeyModifiers::CONTROL), false),
            Action::SplitVertical
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
    fn alt_arrow_moves_focus() {
        assert_eq!(
            resolve(&key(KeyCode::Left, KeyModifiers::ALT), false),
            Action::FocusLeft
        );
    }
}
