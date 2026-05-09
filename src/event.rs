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
    let auto_scroll_interval = Duration::from_millis(30);
    let mut last_refresh = Instant::now();
    let mut last_auto_scroll = Instant::now();
    let refresh_every = Duration::from_secs(2);
    let mut pane_rects: HashMap<PaneId, Rect> = HashMap::new();
    let mut sidebar_file_rect: Option<Rect> = None;
    let mut tab_rects: Vec<(Rect, usize)> = Vec::new();

    while !app.quit {
        term.draw(|f| {
            crate::ui::draw(
                &app,
                f,
                &mut pane_rects,
                &mut sidebar_file_rect,
                &mut tab_rects,
            )
        })?;

        // ドラッグ端 auto-scroll: ペイン外側まで持っていかれたドラッグを 30ms 毎に
        // 1 行ずつ scroll しながら anchor を内容に追従させる。これによりスクロール
        // バック内 (上端外) や未表示の最新行 (下端外) を 1 ドラッグで選択できる。
        if app
            .selection
            .as_ref()
            .is_some_and(|s| s.dragging && s.auto_scroll != 0)
            && last_auto_scroll.elapsed() >= auto_scroll_interval
        {
            advance_drag_auto_scroll(&mut app);
            last_auto_scroll = Instant::now();
        }

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
            process_batch(&mut app, batch, &pane_rects, sidebar_file_rect, &tab_rects)?;
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
    tab_rects: &[(Rect, usize)],
) -> Result<()> {
    let mut i = 0;
    while i < events.len() {
        // 連続する paste 系イベント (Event::Paste と classify_run でマッチする
        // Char/Enter/Tab run) をまとめて 1 回の handle_paste にする。
        // Windows ConPTY が大きいペーストを複数チャンク=複数 Event::Paste で
        // 配信するため、ここで合流させないと Claude 側で N 個の placeholder に
        // 分裂して見える。
        if let Some((consumed, text)) = collect_paste_segment(&events[i..]) {
            handle_paste(app, &text);
            i += consumed;
            continue;
        }

        match &events[i] {
            // KeyEventKind::Press のみ処理する。Windows ConPTY が wRepeatCount > 1 や
            // KeyEventKind::Repeat を発行することがあり、Backspace 1 回で 2-3 文字
            // 消える / Shift+Tab を 1 回押しただけで Claude のモードが 2 回切り替わる
            // (= 元に戻る) といった「1 押下 = N 送信」バグが起きる。Repeat はここで
            // 落とし、ホールド時のリピートは子プロセス (claude / cmd.exe) 側に任せる。
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                handle_key(app, *key, pane_rects)?;
            }
            Event::Mouse(me) => {
                handle_mouse(app, *me, pane_rects, sidebar_file_rect, tab_rects);
            }
            Event::Resize(_, _) => {}
            _ => {}
        }
        i += 1;
    }
    Ok(())
}

/// 先頭から paste セグメント (Event::Paste と classify_run マッチを連結) を
/// 1 つに合流させて返す。`Event::Paste` 単体でも 1 segment として扱うので、
/// チャンク分割された bracketed-paste も 1 ペーストにまとまる。
fn collect_paste_segment(events: &[Event]) -> Option<(usize, String)> {
    let started_with_paste = matches!(events.first(), Some(Event::Paste(_)));
    let started_with_run = classify_run(events).is_some();
    if !started_with_paste && !started_with_run {
        return None;
    }
    let mut text = String::new();
    let mut i = 0;
    while i < events.len() {
        match &events[i] {
            Event::Paste(t) => {
                text.push_str(t);
                i += 1;
            }
            _ => {
                if let Some((consumed, run_text)) = classify_run(&events[i..]) {
                    text.push_str(&run_text);
                    i += consumed;
                } else {
                    break;
                }
            }
        }
    }
    if i == 0 {
        None
    } else {
        Some((i, text))
    }
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
/// `Event::Paste` (bracketed-paste 由来) も大きいペーストが複数チャンクに分割
/// されるケースに備えて run の継続として扱う。
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
        Event::Paste(_) => true,
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

fn scroll_focused(app: &mut App, delta: i32) {
    let focused_id = app.current_tab().focused;
    scroll_pane_with_selection(app, focused_id, delta);
}

/// `pane.scroll_by` を呼びつつ、同じペインに非ドラッグの選択がある場合は
/// 実際に変化した scrollback 量ぶん anchor/cursor の row を平行移動させる。
/// これによりスクロール後も「選択中のテキスト」と反転帯がズレない。
///
/// scrollback クランプで実際は動かなかったケース (actual == 0) では selection も
/// 動かさない。ドラッグ中 (`sel.dragging == true`) は `advance_drag_auto_scroll`
/// が anchor のみ追従させる別ロジックを担当しているのでここでは触らない。
fn scroll_pane_with_selection(app: &mut App, pane_id: PaneId, delta: i32) {
    let Some(pane) = app.panes.get(&pane_id) else {
        return;
    };
    let before = pane
        .parser
        .lock()
        .ok()
        .map(|p| p.screen().scrollback() as i32)
        .unwrap_or(0);
    pane.scroll_by(delta);
    let after = pane
        .parser
        .lock()
        .ok()
        .map(|p| p.screen().scrollback() as i32)
        .unwrap_or(before);
    let actual = after - before;
    if actual == 0 {
        return;
    }
    if let Some(sel) = app.selection.as_mut() {
        if sel.pane_id == pane_id && !sel.dragging {
            sel.anchor.1 += actual;
            sel.cursor.1 += actual;
        }
    }
}

/// ドラッグ中の auto-scroll を 1 行進める。
///
/// `selection.auto_scroll` の符号:
/// - `+1` (下端外) → 新しい行を見たい → vt100 の scrollback offset を `-1`。
/// - `-1` (上端外) → 古い行を見たい → vt100 の scrollback offset を `+1`。
///
/// 実際に変化した scrollback 量と同じだけ anchor.row を補正することで、
/// スクロール後も「最初にクリックした内容の行」を anchor が指し続ける
/// (画面外に出れば負値になる)。cursor は端に張り付いたままなので調整しない。
///
/// 「実際の」変化量を使うのが重要: scrollback=0 で下端ドラッグ→新しい行を
/// 要求しても vt100 はクランプして scroll しないが、要求 delta で anchor を
/// 動かしてしまうと反転帯だけが上方向に伸びていく不具合になる。
fn advance_drag_auto_scroll(app: &mut App) {
    let Some(sel) = app.selection.as_mut() else {
        return;
    };
    if !sel.dragging || sel.auto_scroll == 0 {
        return;
    }
    let scroll_delta = -sel.auto_scroll;
    let pane_id = sel.pane_id;
    let Some(pane) = app.panes.get(&pane_id) else {
        return;
    };
    let before = pane
        .parser
        .lock()
        .ok()
        .map(|p| p.screen().scrollback() as i32)
        .unwrap_or(0);
    pane.scroll_by(scroll_delta);
    let after = pane
        .parser
        .lock()
        .ok()
        .map(|p| p.screen().scrollback() as i32)
        .unwrap_or(before);
    let actual = after - before;
    if actual == 0 {
        return;
    }
    sel.anchor.1 += actual;
}

fn handle_mouse(
    app: &mut App,
    me: MouseEvent,
    pane_rects: &HashMap<PaneId, Rect>,
    sidebar_file_rect: Option<Rect>,
    tab_rects: &[(Rect, usize)],
) {
    use crossterm::event::{MouseButton, MouseEventKind::*};
    let mx = me.column as i32;
    let my = me.row as i32;

    match me.kind {
        ScrollUp | ScrollDown => {
            let target = pane_rects
                .iter()
                .find_map(|(pid, r)| {
                    (mx >= r.x && mx < r.x + r.w && my >= r.y && my < r.y + r.h).then_some(*pid)
                })
                .unwrap_or(app.current_tab().focused);
            if me.modifiers.contains(KeyModifiers::CONTROL) {
                // Ctrl + Wheel: target ペインのサイズを段階的に増減。
                // 直近親 Split の ratio を ±0.05 動かす (target 側が大きく
                // なる方向に揃える)。次フレームの ui::draw() で pty.resize
                // が呼ばれ Claude Code 側にも自動でサイズ変更が伝わる。
                let delta = if matches!(me.kind, ScrollUp) {
                    0.05
                } else {
                    -0.05
                };
                app.current_tab_mut().layout.adjust_ratio_for(target, delta);
            } else {
                let delta = if matches!(me.kind, ScrollUp) { 1 } else { -1 };
                scroll_pane_with_selection(app, target, delta * 3);
            }
        }
        Down(MouseButton::Left) => {
            // タブバー上のクリック → アクティブタブ切替。リネーム中は無視。
            if app.renaming_tab.is_none() {
                for (rect, idx) in tab_rects {
                    if mx >= rect.x && mx < rect.x + rect.w && my >= rect.y && my < rect.y + rect.h
                    {
                        app.active_tab = *idx;
                        app.selection = None;
                        return;
                    }
                }
            }
            // Ctrl+Left クリック → クリック位置の URL をデフォルトブラウザで開く。
            // 通常クリック (選択開始) より優先。
            if me.modifiers.contains(KeyModifiers::CONTROL) {
                if let Some((pid, rect)) = find_pane_at(pane_rects, mx, my) {
                    let lx = (mx - rect.x).clamp(0, rect.w.saturating_sub(1)) as u16;
                    let ly = (my - rect.y).clamp(0, rect.h.saturating_sub(1)) as u16;
                    if let Some(pane) = app.panes.get(&pid) {
                        if let Some(url) = url_at_cell(pane, lx, ly) {
                            open_url(&url);
                            app.selection = None;
                            return;
                        }
                    }
                }
            }
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
                        if let Some(crate::sidebar::filetree::ActivateResult::File(p)) =
                            app.sidebar.file_tree.activate_at(row)
                        {
                            let editor =
                                std::env::var("EDITOR").unwrap_or_else(|_| "code".to_string());
                            let _ = std::process::Command::new(editor).arg(&p).spawn();
                        }
                        app.selection = None;
                        return;
                    }
                }
            }
            // 新しいドラッグ選択を開始。クリック位置が pane 内なら selection を更新、
            // 外(サイドバー/タブバー/境界)なら既存選択はクリアする。
            // ダブルクリック判定 (同じペイン・同じ row で 500ms 以内の再クリック) は
            // その行を全選択する (コピペ用途)。
            if let Some((pid, rect)) = find_pane_at(pane_rects, mx, my) {
                let lx = (mx - rect.x).clamp(0, rect.w.saturating_sub(1)) as u16;
                let ly = (my - rect.y).clamp(0, rect.h.saturating_sub(1)) as u16;
                let now = Instant::now();
                let is_double = matches!(
                    app.last_left_click,
                    Some((t, last_pid, last_ly))
                        if last_pid == pid
                            && last_ly == ly
                            && now.duration_since(t) <= Duration::from_millis(500)
                );
                if is_double {
                    // 行全体ではなく「実テキスト範囲」だけを選択する。先頭の
                    // whitespace / bullet (●○•・*-+) / 番号 (5.) / プロンプト (>❯▶)
                    // をスキップし、末尾の trailing whitespace を除く。これにより
                    // Claude Code のリスト表示行をダブルクリックしたときに `> 5. ●`
                    // のマーカーが反転対象に入らず、コピー結果も本文だけになる。
                    let range = app.panes.get(&pid).and_then(|pane| {
                        pane.parser
                            .lock()
                            .ok()
                            .and_then(|parser| line_text_range(parser.screen(), ly))
                    });
                    if let Some((start_x, end_x)) = range {
                        app.selection = Some(crate::app::Selection {
                            pane_id: pid,
                            anchor: (start_x, ly as i32),
                            cursor: (end_x, ly as i32),
                            dragging: false,
                            auto_scroll: 0,
                        });
                    } else {
                        // 空行など実テキストが無いときは選択しない。
                        app.selection = None;
                    }
                    // 連続トリプル化を防ぐためリセット。
                    app.last_left_click = None;
                } else {
                    app.selection = Some(crate::app::Selection {
                        pane_id: pid,
                        anchor: (lx, ly as i32),
                        cursor: (lx, ly as i32),
                        dragging: true,
                        auto_scroll: 0,
                    });
                    app.last_left_click = Some((now, pid, ly));
                }
            } else {
                app.selection = None;
                app.last_left_click = None;
            }
        }
        Drag(MouseButton::Left) => {
            if let Some(sel) = app.selection.as_mut() {
                // ダブルクリックで作られた行選択 (dragging=false) は微細なマウス
                // ジッタで壊さない。手動ドラッグ中 (dragging=true) のみ cursor 追従。
                if sel.dragging {
                    if let Some(rect) = pane_rects.get(&sel.pane_id) {
                        let lx = (mx - rect.x).clamp(0, rect.w.saturating_sub(1)) as u16;
                        // ペイン外側 (上端より上 / 下端より下) では cursor を端に
                        // 張り付かせつつ auto_scroll 方向を立てる。tick 駆動で
                        // 1 行/30ms スクロールしながら anchor を追従させる。
                        let (ly, dir) = if my < rect.y {
                            (0i32, -1)
                        } else if my >= rect.y + rect.h {
                            (rect.h.saturating_sub(1).max(0), 1)
                        } else {
                            (my - rect.y, 0)
                        };
                        sel.cursor = (lx, ly);
                        sel.auto_scroll = dir;
                    }
                }
            }
        }
        Up(MouseButton::Left) => {
            if let Some(sel) = app.selection.as_mut() {
                sel.dragging = false;
                sel.auto_scroll = 0;
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

/// クリック位置 (col,row) に重なる URL を、その行の vt100 セル列から検出して返す。
/// 行内検出のみ (折り返し URL は対象外)。`http://` / `https://` をスキャンし、空白か
/// 制御文字に当たるまで取り込む。末尾の句読点 (.,;:!?)]>") は URL 外として削る。
fn url_at_cell(pane: &crate::pane::Pane, col: u16, row: u16) -> Option<String> {
    let parser = pane.parser.lock().ok()?;
    let screen = parser.screen();
    let (_rows, cols) = screen.size();

    // 行の文字列を (char, 表示列) 列として展開。空セルは半角スペース扱い。
    // ワイド文字や合成は同じ表示列に複数 char が乗ることがあるため pair で持つ。
    let mut chars: Vec<(char, u16)> = Vec::new();
    for x in 0..cols {
        let contents = screen
            .cell(row, x)
            .map(|c| c.contents())
            .unwrap_or_default();
        if contents.is_empty() {
            chars.push((' ', x));
        } else {
            for ch in contents.chars() {
                chars.push((ch, x));
            }
        }
    }

    let n = chars.len();
    let mut i = 0;
    while i < n {
        if i + 4 > n {
            break;
        }
        let starts_http = chars[i].0.eq_ignore_ascii_case(&'h')
            && chars[i + 1].0.eq_ignore_ascii_case(&'t')
            && chars[i + 2].0.eq_ignore_ascii_case(&'t')
            && chars[i + 3].0.eq_ignore_ascii_case(&'p');
        if !starts_http {
            i += 1;
            continue;
        }
        let scheme_end = if i + 8 <= n
            && chars[i + 4].0.eq_ignore_ascii_case(&'s')
            && chars[i + 5].0 == ':'
            && chars[i + 6].0 == '/'
            && chars[i + 7].0 == '/'
        {
            i + 8
        } else if i + 7 <= n
            && chars[i + 4].0 == ':'
            && chars[i + 5].0 == '/'
            && chars[i + 6].0 == '/'
        {
            i + 7
        } else {
            i += 1;
            continue;
        };

        let mut end = scheme_end;
        while end < n {
            let c = chars[end].0;
            if c.is_whitespace() || c.is_control() {
                break;
            }
            end += 1;
        }
        // 末尾の句読点を URL 外として削る (URL の直後の `.` `,` `)` 等)。
        while end > scheme_end {
            let c = chars[end - 1].0;
            if matches!(
                c,
                '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '>' | '"' | '\''
            ) {
                end -= 1;
            } else {
                break;
            }
        }

        if end > i {
            let col_start = chars[i].1;
            let col_end = chars[end - 1].1;
            if col >= col_start && col <= col_end {
                let url: String = chars[i..end].iter().map(|(c, _)| *c).collect();
                return Some(url);
            }
            i = end;
        } else {
            i += 1;
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn open_url(url: &str) {
    // `cmd /C start "" <url>` は ShellExecute 経由で既定ブラウザを起動する。
    // 第 2 引数の空文字列は start のタイトル引数で、URL に空白がある場合に
    // タイトルとして食われるのを防ぐためのプレースホルダ。
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
}

#[cfg(target_os = "macos")]
fn open_url(url: &str) {
    let _ = std::process::Command::new("open").arg(url).spawn();
}

#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
fn open_url(url: &str) {
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// 選択範囲内のテキストを vt100 スクリーンから抜き出す。行末のスペースは
/// trim し、行間は '\n' で結合。
///
/// 選択開始 (anchor) がドラッグ中の auto-scroll で表示外 (row < 0) に追い出されて
/// いるケースに対応するため、必要に応じて vt100 の scrollback offset を一時的に
/// 動かして該当行を可視化し、抜き出し終わったら元の offset に復元する。
fn extract_selected_text(pane: &crate::pane::Pane, sel: crate::app::Selection) -> String {
    let Ok(mut parser) = pane.parser.lock() else {
        return String::new();
    };
    extract_selected_text_from_parser(&mut parser, sel)
}

fn extract_selected_text_from_parser(
    parser: &mut vt100::Parser,
    sel: crate::app::Selection,
) -> String {
    let saved_scrollback = parser.screen().scrollback() as i32;
    let (start, end) = normalize_range(sel.anchor, sel.cursor);

    let mut lines: Vec<String> = Vec::new();
    let mut row = start.1;

    // 各反復で「row が画面 y=0 に来るような scrollback offset」をセットし、
    // 連続する可視行 (最大 h 行) を読み取って row を進める。
    // row > end.1 になるか、scrollback floor (= 0) でこれ以上進めなくなったら終了。
    while row <= end.1 {
        let want_scrollback = saved_scrollback - row;
        let actual_scrollback = want_scrollback.max(0);
        parser.set_scrollback(actual_scrollback as usize);
        let screen = parser.screen();
        let (h_now, cols_now) = screen.size();
        let h_now = h_now as i32;
        // 現在の scrollback で screen y=0 が指す「論理 row」(saved 時点での row)。
        let base = saved_scrollback - actual_scrollback;
        // この iteration で読み始める screen y。
        let start_y = (row - base).max(0);
        let mut progressed = false;
        let mut y = start_y;
        while y < h_now {
            let our_row = base + y;
            if our_row > end.1 {
                break;
            }
            if our_row < row {
                y += 1;
                continue;
            }
            let (x0, x1) = if start.1 == end.1 {
                (start.0, end.0)
            } else if our_row == start.1 {
                (start.0, cols_now.saturating_sub(1))
            } else if our_row == end.1 {
                (0, end.0)
            } else {
                (0, cols_now.saturating_sub(1))
            };
            let mut line = String::new();
            let y_u16 = y as u16;
            let upper = x1.min(cols_now.saturating_sub(1));
            if x0 <= upper {
                for x in x0..=upper {
                    if let Some(cell) = screen.cell(y_u16, x) {
                        // 全角文字 (CJK 等 wide char) の第 2 セルは contents() が空。
                        // 空セル全部を ' ' 詰めにすると「こ ん ち は」のように全角間に
                        // 半角スペースが混入するため、wide-continuation セルは skip。
                        if cell.is_wide_continuation() {
                            continue;
                        }
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
            }
            lines.push(line.trim_end().to_string());
            row = our_row + 1;
            progressed = true;
            y += 1;
        }
        if !progressed {
            // scrollback 床に達してもまだ end.1 に届かない異常系。安全のため break。
            break;
        }
    }

    parser.set_scrollback(saved_scrollback.max(0) as usize);
    lines.join("\n")
}

/// 行 y の「実テキスト範囲」を返す。
///
/// 先頭の whitespace / 箇条書きマーカー (●○•・*-+) / 番号 (`5.`) /
/// プロンプト記号 (`>` `❯` `▶`) を skip し、末尾の trailing whitespace を
/// 除いた最終可視 col までを `(start_x, end_x)` で返す。
/// 行が空 (実テキストなし) の場合は `None`。
///
/// ダブルクリック時に Claude Code 風のリスト表示行 `> 5. ●こんちは…` から
/// 「こんちは…」だけを選択するために使う。
fn line_text_range(screen: &vt100::Screen, y: u16) -> Option<(u16, u16)> {
    let (rows, cols) = screen.size();
    if y >= rows || cols == 0 {
        return None;
    }
    let last_col = cols.saturating_sub(1);

    // 末尾の trailing whitespace を skip。空セル (contents 空) や wide-continuation
    // も実体なしとして skip する。
    let mut end_x = last_col;
    loop {
        let is_blank = match screen.cell(y, end_x) {
            None => true,
            Some(cell) if cell.is_wide_continuation() => true,
            Some(cell) => {
                let s = cell.contents();
                s.is_empty() || s.chars().all(|c| c.is_whitespace())
            }
        };
        if !is_blank {
            break;
        }
        if end_x == 0 {
            return None;
        }
        end_x -= 1;
    }

    // 先頭からマーカー文字を skip。
    let mut start_x: u16 = 0;
    while start_x <= end_x {
        let cell = screen.cell(y, start_x);
        let skip = match cell {
            None => true,
            Some(c) if c.is_wide_continuation() => true,
            Some(c) => {
                let s = c.contents();
                if s.is_empty() {
                    true
                } else {
                    s.chars().next().is_some_and(is_leading_marker_char)
                }
            }
        };
        if skip {
            start_x += 1;
        } else {
            break;
        }
    }

    if start_x > end_x {
        return None;
    }
    Some((start_x, end_x))
}

/// 行頭でマーカーとして skip する文字判定。bullet, ASCII 数字 + 句読点,
/// プロンプト矢印, ASCII 空白 + 全角空白を含む。本文に普通に出てくる
/// 漢字・かな・英字・記号 (`!?…` 等) は skip 対象にしない。
fn is_leading_marker_char(c: char) -> bool {
    matches!(
        c,
        ' '
        | '\t'
        | '\u{00a0}' // NBSP
        | '\u{3000}' // 全角スペース
        | '>' | '❯' | '▶' | '▷' | '►' | '»'
        | '●' | '○' | '•' | '・' | '◯' | '◦' | '◉' | '◎'
        | '*' | '+' | '-' | '–' | '—' | '─'
        | '0'..='9' | '.' | ',' | ':' | ')' | ']' | '|' | '#'
    )
}

/// (anchor, cursor) を行優先で昇順に並べ替える。
fn normalize_range(a: (u16, i32), b: (u16, i32)) -> ((u16, i32), (u16, i32)) {
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

    #[test]
    fn collect_paste_segment_merges_consecutive_paste_events() {
        // バグ再現: Windows ConPTY が大きい paste を 3 つの Event::Paste に分割。
        // 1 つの handle_paste にまとまるよう、3 chunk を 1 segment として返す。
        let events = vec![
            Event::Paste("hello\n".to_string()),
            Event::Paste("world\n".to_string()),
            Event::Paste("!".to_string()),
        ];
        assert_eq!(
            collect_paste_segment(&events),
            Some((3, "hello\nworld\n!".to_string()))
        );
    }

    #[test]
    fn collect_paste_segment_single_paste_event() {
        let events = vec![Event::Paste("abc".to_string())];
        assert_eq!(collect_paste_segment(&events), Some((1, "abc".to_string())));
    }

    #[test]
    fn collect_paste_segment_classify_run_only() {
        let events = vec![
            press(KeyCode::Char('a')),
            press(KeyCode::Char('b')),
            press(KeyCode::Enter),
        ];
        assert_eq!(
            collect_paste_segment(&events),
            Some((3, "ab\n".to_string()))
        );
    }

    #[test]
    fn collect_paste_segment_mixed_paste_event_and_run() {
        // bracketed-paste チャンク → ConPTY が key event 列に切り替えても続けて貼る。
        let events = vec![
            Event::Paste("foo".to_string()),
            press(KeyCode::Char('b')),
            press(KeyCode::Char('a')),
            press(KeyCode::Char('r')),
            press(KeyCode::Enter),
        ];
        assert_eq!(
            collect_paste_segment(&events),
            Some((5, "foobar\n".to_string()))
        );
    }

    #[test]
    fn collect_paste_segment_returns_none_for_non_paste() {
        let events = vec![press(KeyCode::Char('x'))];
        assert_eq!(collect_paste_segment(&events), None);
    }

    #[test]
    fn collect_paste_segment_stops_at_non_paste_event() {
        let events = vec![
            Event::Paste("hi".to_string()),
            Event::Resize(80, 24),
            Event::Paste("ignored-by-segment".to_string()),
        ];
        assert_eq!(collect_paste_segment(&events), Some((1, "hi".to_string())));
    }

    fn screen_from(rows: u16, cols: u16, text: &str) -> vt100::Parser {
        let mut parser = vt100::Parser::new(rows, cols, 0);
        parser.process(text.as_bytes());
        parser
    }

    #[test]
    fn line_text_range_skips_bullet_and_numbering() {
        // Claude Code 風の "> 5. ●こんちは！今日は何をやりましょうか？" 行を再現。
        let parser = screen_from(2, 60, "> 5. ●こんちは！今日は何をやりましょうか？");
        let (start, end) = line_text_range(parser.screen(), 0).expect("range");
        // "> 5. ●" を skip して "こ" の cell から開始するはず。
        let head = parser
            .screen()
            .cell(0, start)
            .map(|c| c.contents())
            .unwrap_or_default();
        assert_eq!(head, "こ", "expected start at こ, got {:?}", head);
        // 末尾は "？" の wide char 第 1 セル。
        let tail = parser
            .screen()
            .cell(0, end)
            .map(|c| c.contents())
            .unwrap_or_default();
        assert_eq!(tail, "？", "expected end at ？, got {:?}", tail);
    }

    #[test]
    fn line_text_range_returns_none_for_empty_line() {
        let parser = screen_from(2, 20, "");
        assert_eq!(line_text_range(parser.screen(), 0), None);
    }

    #[test]
    fn line_text_range_returns_none_for_whitespace_only() {
        let parser = screen_from(2, 20, "          ");
        assert_eq!(line_text_range(parser.screen(), 0), None);
    }

    #[test]
    fn line_text_range_returns_none_for_marker_only() {
        // 行全体が bullet/whitespace で実テキストが無いケース。
        let parser = screen_from(2, 20, "  - ");
        assert_eq!(line_text_range(parser.screen(), 0), None);
    }

    #[test]
    fn line_text_range_keeps_text_starting_with_alphabet() {
        let parser = screen_from(2, 30, "hello world");
        let (start, end) = line_text_range(parser.screen(), 0).expect("range");
        assert_eq!(start, 0);
        assert_eq!(end, 10);
    }

    #[test]
    fn is_leading_marker_char_recognizes_common_markers() {
        for c in [' ', '\t', '●', '○', '・', '•', '-', '*', '+', '>', '❯', '#'] {
            assert!(is_leading_marker_char(c), "expected marker: {:?}", c);
        }
        for c in [
            '0', '1', '5', '9', '.', ':', // numbering / list-item separators
        ] {
            assert!(is_leading_marker_char(c), "expected marker: {:?}", c);
        }
    }

    #[test]
    fn is_leading_marker_char_rejects_text_chars() {
        for c in ['こ', 'a', 'A', 'あ', '漢', '！', '？', '/', '\\'] {
            assert!(!is_leading_marker_char(c), "expected non-marker: {:?}", c);
        }
    }

    /// rows = 5 の vt100 にスクロールバックを発生させた上で、anchor を可視外
    /// (row < 0 = スクロールバック内) に置いた選択範囲が scrollback 内の行も
    /// 正しくコピーできることを確認する。下端ドラッグ auto-scroll が完了した
    /// 直後の状態に相当。
    #[test]
    fn extract_selected_text_walks_into_scrollback() {
        let mut parser = vt100::Parser::new(5, 20, 100);
        // line01..line11 を改行付きで、line12 を最後に改行なしで流す。
        // こうすると line12 が live grid の row 4 に座る (末尾空行なし)。
        // 結果: 可視 5 行 = [line08, line09, line10, line11, line12],
        //       scrollback = [line01..line07] (7 行)。
        let mut payload = String::new();
        for i in 1..=11 {
            payload.push_str(&format!("line{:02}\r\n", i));
        }
        payload.push_str("line12");
        parser.process(payload.as_bytes());

        // 可視 top (y=0) = line08 なので、anchor row=-3 → line05、
        // cursor row=4 → line12 を選択範囲に含める。
        let sel = crate::app::Selection {
            pane_id: 1,
            anchor: (0, -3),
            cursor: (19, 4),
            dragging: false,
            auto_scroll: 0,
        };
        let text = extract_selected_text_from_parser(&mut parser, sel);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines,
            vec!["line05", "line06", "line07", "line08", "line09", "line10", "line11", "line12",],
            "got: {:#?}",
            lines
        );
        // scrollback offset は元 (0) に復元されているはず。
        assert_eq!(parser.screen().scrollback(), 0);
    }

    /// 全選択範囲が現在の可視内に収まる通常ケース (回帰テスト)。
    #[test]
    fn extract_selected_text_handles_visible_only_selection() {
        let mut parser = vt100::Parser::new(5, 20, 100);
        parser.process(b"alpha\r\nbeta\r\ngamma\r\n");
        let sel = crate::app::Selection {
            pane_id: 1,
            anchor: (0, 0),
            cursor: (19, 2),
            dragging: false,
            auto_scroll: 0,
        };
        let text = extract_selected_text_from_parser(&mut parser, sel);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines, vec!["alpha", "beta", "gamma"]);
    }
}
