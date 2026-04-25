use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent};
use ratatui::backend::Backend;
use ratatui::Terminal;

use crate::app::{App, Rect};
use crate::keymap::{resolve, Action};
use crate::pane::grid::Direction;
use crate::pane::PaneId;
use crate::sidebar::Section;

pub fn run_event_loop<B: Backend>(term: &mut Terminal<B>, mut app: App) -> Result<()> {
    let tick = Duration::from_millis(30);
    let mut last_refresh = Instant::now();
    let refresh_every = Duration::from_secs(2);
    let mut pane_rects: HashMap<PaneId, Rect> = HashMap::new();
    let mut sidebar_file_rect: Option<Rect> = None;

    while !app.quit {
        term.draw(|f| {
            crate::ui::draw(&app, f, &mut pane_rects, &mut sidebar_file_rect)
        })?;

        if event::poll(tick)? {
            // 同一 tick 内に溜まっているイベントを一気に drain して batch 化する。
            // ペーストは Windows 上で Event::Paste ではなく個別 Key イベント群として
            // 届くことがあるため、batch を走査して Enter を含む複数文字の連続を
            // paste として検出・束ねる (Event::Paste が発火する環境でも従来どおり動く)。
            let mut batch: Vec<Event> = vec![event::read()?];
            while event::poll(Duration::from_millis(0))? {
                batch.push(event::read()?);
            }
            // batch 末尾が paste 候補のままで終わっている場合、Windows ConPTY が
            // 1 回の paste を複数 tick に分割して届けている可能性がある。
            // burst が継続する限り (5ms 間隔で次イベントが到着する限り) 最大 500ms
            // まで集め続けて 1 batch に統合する。これにより Claude CLI に届く
            // bracketed-paste セグメントが 1 個になり、placeholder が分裂しない。
            // 5ms 間隔のしきい値は人間のタイピング (≥ 50ms 間隔) と十分離れている
            // ため誤検知しない。
            let burst_deadline = Instant::now() + Duration::from_millis(500);
            while Instant::now() < burst_deadline && batch.last().is_some_and(is_paste_candidate) {
                if event::poll(Duration::from_millis(5))? {
                    batch.push(event::read()?);
                } else {
                    break;
                }
            }
            process_batch(&mut app, batch, &pane_rects, sidebar_file_rect)?;
        }

        if last_refresh.elapsed() >= refresh_every {
            app.sidebar.refresh();
            last_refresh = Instant::now();
        }
    }
    Ok(())
}

fn process_batch(
    app: &mut App,
    events: Vec<Event>,
    pane_rects: &HashMap<PaneId, Rect>,
    sidebar_file_rect: Option<Rect>,
) -> Result<()> {
    let mut i = 0;
    while i < events.len() {
        if let Some((consumed, text)) = classify_run(&events[i..]) {
            handle_paste(app, &text);
            i += consumed;
            continue;
        }

        match &events[i] {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                handle_key(app, *key, pane_rects)?;
            }
            Event::Mouse(me) => {
                handle_mouse(app, *me, pane_rects, sidebar_file_rect);
            }
            Event::Paste(text) => {
                handle_paste(app, text);
            }
            Event::Resize(_, _) => {}
            _ => {}
        }
        i += 1;
    }
    Ok(())
}

/// 先頭から paste run を切り出す純粋関数。
///
/// Char/Enter/Tab(Ctrl/Alt なし)が連続する区間を run とし、Press と Release が
/// 交互に届く Windows ConPTY のケースでも途切れないよう Release も run の一部として
/// 含める。Press イベント数が 2 以上かつ Enter Press を含むときのみ paste 判定し、
/// `(消費イベント数, paste テキスト)` を返す。Press 数で判定するのは生 run_len が
/// Press/Release ペアで膨らむため (Press+Release = 2 events で 1 文字)。
fn classify_run(events: &[Event]) -> Option<(usize, String)> {
    let mut run_end = 0;
    while run_end < events.len() && is_paste_candidate(&events[run_end]) {
        run_end += 1;
    }
    if run_end == 0 {
        return None;
    }
    let mut press_count = 0usize;
    let mut has_enter = false;
    for e in &events[..run_end] {
        if let Event::Key(k) = e {
            if k.kind != KeyEventKind::Release {
                press_count += 1;
                if matches!(k.code, KeyCode::Enter) {
                    has_enter = true;
                }
            }
        }
    }
    if press_count < 2 || !has_enter {
        return None;
    }
    let text: String = events[..run_end]
        .iter()
        .filter_map(key_to_paste_char)
        .collect();
    Some((run_end, text))
}

/// Key イベントが paste の一部になり得るか判定する。
/// Ctrl/Alt 修飾付きキーやファンクションキーは paste に束ねない。
/// Press / Release のいずれも run の継続を許す (Windows ConPTY が交互に届けるため)。
fn is_paste_candidate(e: &Event) -> bool {
    match e {
        Event::Key(k) => {
            if k.modifiers.contains(KeyModifiers::CONTROL)
                || k.modifiers.contains(KeyModifiers::ALT)
            {
                return false;
            }
            matches!(k.code, KeyCode::Char(_) | KeyCode::Enter | KeyCode::Tab)
        }
        _ => false,
    }
}

fn key_to_paste_char(e: &Event) -> Option<String> {
    match e {
        Event::Key(k) if k.kind != KeyEventKind::Release => match k.code {
            KeyCode::Char(c) => Some(c.to_string()),
            KeyCode::Enter => Some("\n".to_string()),
            KeyCode::Tab => Some("\t".to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn handle_key(app: &mut App, key: KeyEvent, pane_rects: &HashMap<PaneId, Rect>) -> Result<()> {
    // Rename mode: intercept everything before keymap resolution.
    if app.renaming_tab.is_some() {
        handle_rename_key(app, key);
        return Ok(());
    }

    // 選択範囲が存在する状態で Ctrl+C → クリップボードへコピーして選択解除。
    // reshell / pass-through より優先。その他のキーは選択をクリアしてから通常処理。
    if let Some(sel) = app.selection {
        if is_ctrl_c(&key) {
            if let Some(pane) = app.panes.get(&sel.pane_id) {
                let text = extract_selected_text(pane, sel);
                if !text.is_empty() {
                    copy_to_clipboard(&text);
                }
            }
            app.selection = None;
            app.last_ctrl_c = None; // 2 連打カウンタもリセット
            return Ok(());
        }
        // Ctrl+C 以外のキーが来たら選択を解除して以降を通常処理。
        app.selection = None;
    }

    // Ctrl+C 2 連打でフォーカス中ペインを shell(cmd.exe / $SHELL) に切り替える。
    // 1 回目は従来どおり子プロセス(claude など)へ 0x03 を pass-through する。
    if is_ctrl_c(&key) {
        let now = Instant::now();
        let focused_id = app.current_tab().focused;
        let double_tap = matches!(
            app.last_ctrl_c,
            Some((pid, t))
                if pid == focused_id && now.duration_since(t) <= Duration::from_millis(800)
        );
        if double_tap {
            if let Some(pane) = app.panes.get_mut(&focused_id) {
                if pane.claude_running {
                    let _ = pane.respawn_as_shell();
                    app.last_ctrl_c = None;
                    return Ok(());
                }
            }
        }
        app.last_ctrl_c = Some((focused_id, now));
        if !app.sidebar_focused {
            if let Some(pane) = app.panes.get(&focused_id) {
                pane.scroll_to_bottom();
                pane.write(&[0x03]);
            }
        }
        return Ok(());
    }

    let action = resolve(&key, app.sidebar_focused);
    match action {
        Action::Quit => app.quit = true,
        Action::SplitHorizontal => {
            app.split(Direction::Down)?;
        }
        Action::SplitVertical => {
            app.split(Direction::Right)?;
        }
        Action::NewTab => {
            app.new_tab()?;
        }
        Action::ClosePane => {
            app.close_focused_pane();
        }
        Action::FocusLeft => app.focus_neighbor(Direction::Left, pane_rects),
        Action::FocusRight => app.focus_neighbor(Direction::Right, pane_rects),
        Action::FocusUp => app.focus_neighbor(Direction::Up, pane_rects),
        Action::FocusDown => app.focus_neighbor(Direction::Down, pane_rects),
        Action::NextTab => app.next_tab(),
        Action::PrevTab => app.prev_tab(),
        Action::ToggleSidebar => {
            app.sidebar.visible = !app.sidebar.visible;
            if !app.sidebar.visible {
                app.sidebar_focused = false;
            }
        }
        Action::ToggleFileTree => {
            toggle_file_tree(app);
        }
        Action::BeginRenameTab => {
            app.renaming_tab = Some(app.current_tab().title.clone());
        }
        Action::SidebarSection(idx) => {
            app.sidebar.visible = true;
            app.sidebar_focused = true;
            app.sidebar.jump_section(idx);
        }
        Action::SidebarCursorUp => {
            let max = current_section_len(app);
            app.sidebar.move_cursor(-1, max);
        }
        Action::SidebarCursorDown => {
            let max = current_section_len(app);
            app.sidebar.move_cursor(1, max);
        }
        Action::SidebarCycleSection => app.sidebar.cycle_section(),
        Action::SidebarOpenEntry => {
            open_selected_entry(app);
        }
        Action::FocusSidebar => {
            app.sidebar.visible = true;
            app.sidebar_focused = true;
        }
        Action::FocusContent => {
            app.sidebar_focused = false;
        }
        Action::ScrollLineUp => scroll_focused(app, 1),
        Action::ScrollLineDown => scroll_focused(app, -1),
        Action::ScrollPageUp => {
            let h = pane_rects
                .get(&app.current_tab().focused)
                .map(|r| r.h)
                .unwrap_or(24);
            scroll_focused(app, h.max(1));
        }
        Action::ScrollPageDown => {
            let h = pane_rects
                .get(&app.current_tab().focused)
                .map(|r| r.h)
                .unwrap_or(24);
            scroll_focused(app, -h.max(1));
        }
        Action::PassThrough => {
            if app.sidebar_focused {
                // Ignore character input while sidebar has focus.
                return Ok(());
            }
            let bytes = key_to_bytes(&key);
            if !bytes.is_empty() {
                if let Some(pane) = app.panes.get(&app.current_tab().focused) {
                    pane.scroll_to_bottom();
                    pane.write(&bytes);
                }
            }
        }
    }
    Ok(())
}

fn current_section_len(app: &App) -> usize {
    match app.sidebar.active {
        crate::sidebar::Section::FileTree => app.sidebar.file_tree.visible_len(),
        crate::sidebar::Section::Claude => app.current_tab().layout.leaves().len(),
        crate::sidebar::Section::Git => {
            if app.sidebar.git_info.is_some() {
                1
            } else {
                0
            }
        }
        crate::sidebar::Section::Panes => app.current_tab().layout.leaves().len(),
    }
}

fn open_selected_entry(app: &mut App) {
    if let Section::FileTree = app.sidebar.active {
        let cursor = app.sidebar.cursor();
        match app.sidebar.file_tree.activate_at(cursor) {
            Some(crate::sidebar::filetree::ActivateResult::File(path)) => {
                let editor = std::env::var("EDITOR").unwrap_or_else(|_| "code".to_string());
                let _ = std::process::Command::new(editor).arg(&path).spawn();
            }
            Some(crate::sidebar::filetree::ActivateResult::DirToggled) | None => {}
        }
    }
}

fn toggle_file_tree(app: &mut App) {
    if !app.sidebar.visible {
        app.sidebar.visible = true;
        app.sidebar.jump_section(Section::FileTree as u8);
        app.sidebar_focused = true;
        return;
    }
    if app.sidebar.active == Section::FileTree && app.sidebar_focused {
        app.sidebar.visible = false;
        app.sidebar_focused = false;
    } else {
        app.sidebar.jump_section(Section::FileTree as u8);
        app.sidebar_focused = true;
    }
}

fn handle_rename_key(app: &mut App, key: KeyEvent) {
    let Some(buf) = app.renaming_tab.as_mut() else {
        return;
    };
    match key.code {
        KeyCode::Enter => {
            let new_title = buf.trim().to_string();
            if !new_title.is_empty() {
                app.current_tab_mut().title = new_title;
            }
            app.renaming_tab = None;
        }
        KeyCode::Esc => {
            app.renaming_tab = None;
        }
        KeyCode::Backspace => {
            buf.pop();
        }
        KeyCode::Char(c)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            buf.push(c);
        }
        _ => {}
    }
}

fn scroll_focused(app: &App, delta: i32) {
    if let Some(pane) = app.panes.get(&app.current_tab().focused) {
        pane.scroll_by(delta);
    }
}

fn handle_mouse(
    app: &mut App,
    me: MouseEvent,
    pane_rects: &HashMap<PaneId, Rect>,
    sidebar_file_rect: Option<Rect>,
) {
    use crossterm::event::{MouseButton, MouseEventKind::*};
    let mx = me.column as i32;
    let my = me.row as i32;

    match me.kind {
        ScrollUp | ScrollDown => {
            let delta = if matches!(me.kind, ScrollUp) { 1 } else { -1 };
            let target = pane_rects
                .iter()
                .find_map(|(pid, r)| {
                    (mx >= r.x && mx < r.x + r.w && my >= r.y && my < r.y + r.h).then_some(*pid)
                })
                .unwrap_or(app.current_tab().focused);
            if let Some(pane) = app.panes.get(&target) {
                pane.scroll_by(delta * 3);
            }
        }
        Down(MouseButton::Left) => {
            // サイドバー Files 領域のクリック → カーソル移動 + Enter と同じ activate を実行。
            // ペイン選択開始ロジックより先に判定し、当てはまれば early return。
            if let Some(rect) = sidebar_file_rect {
                if mx >= rect.x && mx < rect.x + rect.w && my >= rect.y && my < rect.y + rect.h {
                    let row = (my - rect.y) as usize;
                    if row < app.sidebar.file_tree.visible_len() {
                        app.sidebar.visible = true;
                        app.sidebar_focused = true;
                        app.sidebar.active = Section::FileTree;
                        app.sidebar.set_cursor(row);
                        match app.sidebar.file_tree.activate_at(row) {
                            Some(crate::sidebar::filetree::ActivateResult::File(p)) => {
                                let editor = std::env::var("EDITOR")
                                    .unwrap_or_else(|_| "code".to_string());
                                let _ = std::process::Command::new(editor).arg(&p).spawn();
                            }
                            _ => {}
                        }
                        app.selection = None;
                        return;
                    }
                }
            }
            // 新しいドラッグ選択を開始。クリック位置が pane 内なら selection を更新、
            // 外(サイドバー/タブバー/境界)なら既存選択はクリアする。
            if let Some((pid, rect)) = find_pane_at(pane_rects, mx, my) {
                let lx = (mx - rect.x).clamp(0, rect.w.saturating_sub(1)) as u16;
                let ly = (my - rect.y).clamp(0, rect.h.saturating_sub(1)) as u16;
                app.selection = Some(crate::app::Selection {
                    pane_id: pid,
                    anchor: (lx, ly),
                    cursor: (lx, ly),
                    dragging: true,
                });
            } else {
                app.selection = None;
            }
        }
        Drag(MouseButton::Left) => {
            if let Some(sel) = app.selection.as_mut() {
                if let Some(rect) = pane_rects.get(&sel.pane_id) {
                    let lx = (mx - rect.x).clamp(0, rect.w.saturating_sub(1)) as u16;
                    let ly = (my - rect.y).clamp(0, rect.h.saturating_sub(1)) as u16;
                    sel.cursor = (lx, ly);
                }
            }
        }
        Up(MouseButton::Left) => {
            if let Some(sel) = app.selection.as_mut() {
                sel.dragging = false;
                // 単一クリック(範囲0)はノーマルクリック扱いで選択破棄。
                if sel.anchor == sel.cursor {
                    app.selection = None;
                }
            }
        }
        _ => {}
    }
}

fn find_pane_at(pane_rects: &HashMap<PaneId, Rect>, mx: i32, my: i32) -> Option<(PaneId, Rect)> {
    pane_rects
        .iter()
        .find(|(_, r)| mx >= r.x && mx < r.x + r.w && my >= r.y && my < r.y + r.h)
        .map(|(pid, r)| (*pid, *r))
}

/// 選択範囲内のテキストを vt100 スクリーンから抜き出す。行末のスペースは
/// trim し、行間は '\n' で結合。
fn extract_selected_text(pane: &crate::pane::Pane, sel: crate::app::Selection) -> String {
    let Ok(parser) = pane.parser.lock() else {
        return String::new();
    };
    let screen = parser.screen();
    let (rows, cols) = screen.size();
    let (start, end) = normalize_range(sel.anchor, sel.cursor);
    let mut lines: Vec<String> = Vec::new();
    let max_y = end.1.min(rows.saturating_sub(1));
    for y in start.1..=max_y {
        let (x0, x1) = if start.1 == end.1 {
            (start.0, end.0)
        } else if y == start.1 {
            (start.0, cols.saturating_sub(1))
        } else if y == end.1 {
            (0, end.0)
        } else {
            (0, cols.saturating_sub(1))
        };
        let mut line = String::new();
        for x in x0..=x1.min(cols.saturating_sub(1)) {
            if let Some(cell) = screen.cell(y, x) {
                let ch = cell.contents();
                if ch.is_empty() {
                    line.push(' ');
                } else {
                    line.push_str(&ch);
                }
            } else {
                line.push(' ');
            }
        }
        lines.push(line.trim_end().to_string());
    }
    lines.join("\n")
}

/// (anchor, cursor) を行優先で昇順に並べ替える。
fn normalize_range(a: (u16, u16), b: (u16, u16)) -> ((u16, u16), (u16, u16)) {
    if a.1 < b.1 || (a.1 == b.1 && a.0 <= b.0) {
        (a, b)
    } else {
        (b, a)
    }
}

fn copy_to_clipboard(text: &str) {
    // arboard は初期化に失敗し得る（ヘッドレス環境など）が、コピーは best-effort。
    if let Ok(mut cb) = arboard::Clipboard::new() {
        let _ = cb.set_text(text.to_string());
    }
}

/// ペースト内容を bracketed-paste マーカー `ESC [200~ ... ESC [201~` で包んで
/// フォーカス中ペインの PTY へ書き込む。Claude CLI 等の bracketed-paste 対応
/// 子プロセスは、この区切りを見て「貼り付け」と認識し、途中に含まれる改行を
/// 送信トリガとして扱わずに `[Pasted text +N lines]` のプレースホルダへまとめる。
fn handle_paste(app: &mut App, text: &str) {
    if app.sidebar_focused {
        return;
    }
    let focused_id = app.current_tab().focused;
    let Some(pane) = app.panes.get(&focused_id) else {
        return;
    };
    pane.scroll_to_bottom();
    let mut buf = Vec::with_capacity(text.len() + 12);
    buf.extend_from_slice(b"\x1b[200~");
    // CRLF を LF に正規化しておく（bracketed-paste 内でも CR が送信扱いになる
    // 実装があり得るため事前に潰す）。
    for ch in text.chars() {
        if ch == '\r' {
            continue;
        }
        let mut tmp = [0u8; 4];
        buf.extend_from_slice(ch.encode_utf8(&mut tmp).as_bytes());
    }
    buf.extend_from_slice(b"\x1b[201~");
    pane.write(&buf);
}

fn is_ctrl_c(k: &KeyEvent) -> bool {
    k.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(k.code, KeyCode::Char('c') | KeyCode::Char('C'))
}

fn key_to_bytes(key: &KeyEvent) -> Vec<u8> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let mut buf = Vec::new();
    match key.code {
        KeyCode::Char(c) => {
            if alt {
                buf.push(0x1b); // ESC prefix for Alt
            }
            if ctrl {
                // Basic Ctrl-letter mapping; uppercase mapped the same way.
                let lower = c.to_ascii_lowercase();
                if lower.is_ascii_alphabetic() {
                    buf.push((lower as u8) - b'a' + 1);
                } else {
                    buf.extend_from_slice(c.to_string().as_bytes());
                }
            } else {
                buf.extend_from_slice(c.to_string().as_bytes());
            }
        }
        KeyCode::Enter => {
            if shift || ctrl {
                // ESC+CR: Claude/Copilot CLI が改行（送信せずに次行）として解釈する標準シーケンス。
                buf.extend_from_slice(b"\x1b\r");
            } else {
                buf.push(b'\r');
            }
        }
        KeyCode::Tab => {
            if shift {
                // Back-tab (CSI Z): Claude CLI の Shift+Tab モード切替が認識する。
                buf.extend_from_slice(b"\x1b[Z");
            } else {
                buf.push(b'\t');
            }
        }
        KeyCode::BackTab => buf.extend_from_slice(b"\x1b[Z"),
        KeyCode::Backspace => buf.push(0x7f),
        KeyCode::Esc => buf.push(0x1b),
        KeyCode::Left => buf.extend_from_slice(b"\x1b[D"),
        KeyCode::Right => buf.extend_from_slice(b"\x1b[C"),
        KeyCode::Up => buf.extend_from_slice(b"\x1b[A"),
        KeyCode::Down => buf.extend_from_slice(b"\x1b[B"),
        KeyCode::Home => buf.extend_from_slice(b"\x1b[H"),
        KeyCode::End => buf.extend_from_slice(b"\x1b[F"),
        KeyCode::PageUp => buf.extend_from_slice(b"\x1b[5~"),
        KeyCode::PageDown => buf.extend_from_slice(b"\x1b[6~"),
        KeyCode::Delete => buf.extend_from_slice(b"\x1b[3~"),
        KeyCode::Insert => buf.extend_from_slice(b"\x1b[2~"),
        _ => {}
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new_with_kind(
            code,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        ))
    }

    fn release(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new_with_kind(
            code,
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ))
    }

    fn ctrl_press(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new_with_kind(
            code,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        ))
    }

    #[test]
    fn classify_run_press_only_with_enter_is_paste() {
        let events = vec![
            press(KeyCode::Char('a')),
            press(KeyCode::Char('b')),
            press(KeyCode::Enter),
        ];
        assert_eq!(classify_run(&events), Some((3, "ab\n".to_string())));
    }

    #[test]
    fn classify_run_press_release_interleaved_is_paste() {
        // 本バグ再現: Windows ConPTY が Press/Release を交互に届けるケース。
        let events = vec![
            press(KeyCode::Char('a')),
            release(KeyCode::Char('a')),
            press(KeyCode::Char('b')),
            release(KeyCode::Char('b')),
            press(KeyCode::Enter),
            release(KeyCode::Enter),
        ];
        assert_eq!(classify_run(&events), Some((6, "ab\n".to_string())));
    }

    #[test]
    fn classify_run_multiline_paste_with_releases() {
        let lines = ["foo", "bar", "baz", "qux", "quux"];
        let mut events = Vec::new();
        for line in &lines {
            for c in line.chars() {
                events.push(press(KeyCode::Char(c)));
                events.push(release(KeyCode::Char(c)));
            }
            events.push(press(KeyCode::Enter));
            events.push(release(KeyCode::Enter));
        }
        let result = classify_run(&events).expect("should be paste");
        assert_eq!(result.0, events.len());
        assert_eq!(result.1.matches('\n').count(), 5);
        assert!(result.1.starts_with("foo\nbar\n"));
    }

    #[test]
    fn classify_run_single_enter_press_release_is_not_paste() {
        let events = vec![press(KeyCode::Enter), release(KeyCode::Enter)];
        assert_eq!(classify_run(&events), None);
    }

    #[test]
    fn classify_run_single_char_press_release_is_not_paste() {
        let events = vec![press(KeyCode::Char('x')), release(KeyCode::Char('x'))];
        assert_eq!(classify_run(&events), None);
    }

    #[test]
    fn classify_run_typing_without_enter_is_not_paste() {
        let events = vec![
            press(KeyCode::Char('a')),
            press(KeyCode::Char('b')),
            press(KeyCode::Char('c')),
        ];
        assert_eq!(classify_run(&events), None);
    }

    #[test]
    fn classify_run_breaks_on_ctrl_modifier() {
        // Ctrl 修飾は run を切る → 後続の Enter は別 run になる。
        let events = vec![
            press(KeyCode::Char('a')),
            ctrl_press(KeyCode::Char('c')),
            press(KeyCode::Enter),
        ];
        // 先頭 run は Char('a') のみ、Enter なしで paste 不成立。
        assert_eq!(classify_run(&events), None);
    }

    #[test]
    fn classify_run_breaks_on_mouse_event() {
        let events = vec![
            press(KeyCode::Char('a')),
            Event::Resize(80, 24),
            press(KeyCode::Char('b')),
            press(KeyCode::Enter),
        ];
        // 先頭 run は Char('a') のみで Enter なし → None。
        assert_eq!(classify_run(&events), None);
    }

    #[test]
    fn classify_run_includes_tab_in_paste() {
        let events = vec![
            press(KeyCode::Char('a')),
            press(KeyCode::Tab),
            press(KeyCode::Char('b')),
            press(KeyCode::Enter),
        ];
        assert_eq!(classify_run(&events), Some((4, "a\tb\n".to_string())));
    }
}
