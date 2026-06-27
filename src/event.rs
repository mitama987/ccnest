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

        // 保留中の plain Up/Down フラッシュ: PAIR_WINDOW 内に対のホイールが
        // 来なければ実キー確定として子へ転送する。poll がタイムアウト (false)
        // でも毎 tick ここを通るので、入力が途絶えても保留が取り残されない。
        if let Some((key, at)) = app.pending_arrow {
            if at.elapsed() > PAIR_WINDOW {
                app.pending_arrow = None;
                handle_key(&mut app, key, &pane_rects)?;
            }
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

/// 単一イベントを診断ログ向けに 1 トークンへ整形する純粋関数。
fn fmt_event(ev: &Event) -> String {
    match ev {
        Event::Key(k) => format!("Key({:?},{:?},{:?})", k.code, k.kind, k.modifiers),
        Event::Mouse(m) => format!("Mouse({:?}@{},{})", m.kind, m.column, m.row),
        Event::Paste(s) => format!("Paste(len={})", s.len()),
        Event::Resize(w, h) => format!("Resize({w}x{h})"),
        Event::FocusGained => "FocusGained".to_string(),
        Event::FocusLost => "FocusLost".to_string(),
    }
}

/// 1 バッチ分の入力イベント列を 1 行へ整形する純粋関数 (IO なし=単体テスト可能)。
/// `pending_arrow=<bool> | <ev> | <ev> ...` 形式。ホイール 1 回転で実機が何を
/// 配信しているか (Mouse(ScrollUp/Down) が来るか / 矢印 Key が来るか / 同一
/// バッチか) と、タブクリックが Down(Left)@col,row でどう届くかを後から確定
/// するための診断用。
fn format_trace_line(events: &[Event], pending_arrow: bool) -> String {
    let mut s = format!("pending_arrow={pending_arrow}");
    for ev in events {
        s.push_str(" | ");
        s.push_str(&fmt_event(ev));
    }
    s
}

/// `CCNEST_INPUT_TRACE` 環境変数を初回だけ評価。未設定なら以降ゼロコスト。
fn input_trace_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("CCNEST_INPUT_TRACE").is_ok())
}

/// 入力トレースが有効なときだけ、1 バッチを `%APPDATA%\ccnest\input-trace.log`
/// (OS データディレクトリ配下) に追記する。既定では即 return し挙動・性能に
/// 一切影響しない。全 IO は best-effort でエラー無視 (診断が本体を壊さない)。
fn trace_input_batch(events: &[Event], pending_arrow: bool) {
    if !input_trace_enabled() {
        return;
    }
    let Some(base) = dirs::data_dir() else {
        return;
    };
    let dir = base.join("ccnest");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("input-trace.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write as _;
        let ts = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f");
        let _ = writeln!(f, "{} {}", ts, format_trace_line(events, pending_arrow));
    }
}

fn process_batch(
    app: &mut App,
    events: Vec<Event>,
    pane_rects: &HashMap<PaneId, Rect>,
    sidebar_file_rect: Option<Rect>,
    tab_rects: &[(Rect, usize)],
) -> Result<()> {
    use crossterm::event::MouseEventKind;
    trace_input_batch(&events, app.pending_arrow.is_some());
    let mut i = 0;
    // Windows ConPTY は alt 画面中のホイール 1 回転を Mouse(ScrollUp|ScrollDown)
    // と、それに付随する plain な Up/Down KeyEvent の両方として (順不同・別バッチで)
    // 配信することがある。この後者「ファントム矢印」が子 (Claude Code) に届くと
    // プロンプト履歴の遡りになってしまう。これを決定論的に握りつぶす:
    //   1. 同一バッチでホイールに隣接する矢印は確定ファントム → 即ドロップ。
    //   2. 直前 PAIR_WINDOW 内にホイールがあれば先行ホイールのファントム → ドロップ。
    //   3. それ以外 (ホイール未到来) の矢印は `app.pending_arrow` に保留し、
    //      対のホイールが来たら破棄、PAIR_WINDOW を超えたら実キーとして転送する
    //      (フラッシュは run_event_loop が毎 tick 行う)。
    // これでホイール先行・ファントム先行・別バッチ・逆順のいずれでも取りこぼさず、
    // タイミング窓ヒューリスティック (旧 wheel_budget) のようにジェスチャ端で
    // 漏れない。なお main.rs はホスト側 alt-scroll を無効化していない
    // (DECSET 1007l は過去にホイールスクロールを壊したため撤回済み) ので、本層が
    // 唯一の防御である。
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
                if is_plain_updown_press(&events[i]) {
                    let now = Instant::now();
                    let adjacent = batch_adjacent_wheel(&events, i);
                    match classify_arrow(
                        app.sidebar_focused,
                        app.renaming_tab.is_some(),
                        key,
                        app.last_wheel_at,
                        PAIR_WINDOW,
                        now,
                        adjacent,
                    ) {
                        ArrowAction::Drop => {
                            // 確定ファントム (同一バッチ隣接 or 直前ホイール随伴)。
                            // 握りつぶし、保留があれば一緒に掃除する。
                            app.pending_arrow = None;
                            i += 1;
                            continue;
                        }
                        ArrowAction::Defer => {
                            // 対のホイール未到来。実キーかファントム先行か未確定
                            // なので保留して run_event_loop のフラッシュに委ねる。
                            // 既存保留は別押下なので実キーとして先に転送する。
                            if let Some((pk, _)) = app.pending_arrow.take() {
                                handle_key(app, pk, pane_rects)?;
                            }
                            app.pending_arrow = Some((*key, now));
                            i += 1;
                            continue;
                        }
                        ArrowAction::Forward => {}
                    }
                }
                // 抑制対象でない実キー。スクロール文脈は終わったとみなし、保留中の
                // 矢印も実キーとして確定フラッシュしてから本キーを処理する。
                if let Some((pk, _)) = app.pending_arrow.take() {
                    handle_key(app, pk, pane_rects)?;
                }
                handle_key(app, *key, pane_rects)?;
            }
            Event::Mouse(me) => {
                if matches!(
                    me.kind,
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                ) {
                    app.last_wheel_at = Some(Instant::now());
                    // 保留中の plain Up/Down はこのホイールのファントムだった
                    // → 破棄する (ホイール先行・別バッチのケースもここで相殺)。
                    app.pending_arrow = None;
                }
                handle_mouse(app, *me, pane_rects, sidebar_file_rect, tab_rects);
            }
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
            let focused_id = app.current_tab().focused;
            // 子の DECCKM 状態を読んで矢印/Home/End の形式 (CSI/SS3) を決める。
            let app_cursor = app
                .panes
                .get(&focused_id)
                .and_then(|p| {
                    p.parser
                        .lock()
                        .ok()
                        .map(|g| g.screen().application_cursor())
                })
                .unwrap_or(false);
            let bytes = key_to_bytes(&key, app_cursor);
            if !bytes.is_empty() {
                if let Some(pane) = app.panes.get(&focused_id) {
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

/// `pane.scroll_by` を呼びつつ、同じペインに選択がある場合は実際に変化した
/// scrollback 量ぶん anchor / cursor を平行移動させる。
///
/// - **非ドラッグ時**: anchor / cursor 両方を補正 → 反転帯が選択テキストに
///   貼り付いたままスクロールできる。
/// - **ドラッグ中**: anchor のみ補正、cursor は据え置き → マウス位置は動いて
///   いないので cursor が指す content がスクロール量ぶん入れ替わり、結果と
///   して選択範囲が「ホイール方向の新しい行」を取り込んで伸びる。`advance_drag_auto_scroll`
///   と同じ哲学。
///
/// scrollback クランプで実際は動かなかったケース (actual == 0) では selection
/// も動かさない（クランプ時の anchor 暴走バグの再発防止）。
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
        if sel.pane_id == pane_id {
            sel.anchor.1 += actual;
            if !sel.dragging {
                sel.cursor.1 += actual;
            }
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

    // フォーカス先ペインの内側アプリがマウス報告を要求しているなら、
    // crossterm の MouseEvent をそのアプリ向けレポートに変換して PTY へ
    // 転送する。転送した (or アプリが掴んでいて飲み込んだ) ら以降の
    // ccnest 内部 UI 処理 (選択/スクロール/タブ等) はスキップする。
    if try_forward_mouse(app, &me, pane_rects) {
        return;
    }

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

/// マウスモードを考慮してイベントをフォーカス先ペインの PTY へ転送する。
///
/// 戻り値 `true` = このイベントは処理済み (転送した / アプリが掴んでいる
/// ので飲み込んだ / ローカルドラッグ進行中で既存アームへ委譲した) なので
/// `handle_mouse` は即 return すべき。`false` = マウスモード非該当なので
/// 従来の ccnest 内部 UI 処理を続行する。
///
/// ジェスチャ判定 (ユーザ要望): マウスモード ON ペインで左ボタンを押したら
/// Down を即転送せず保留し、次が別セルへの Drag なら ccnest ローカルの
/// テキスト選択 (Shift 不要)、次が同セルでの Up ならクリック確定として
/// アプリへ合成 Down+Up を転送する。Shift / Ctrl は強制ローカル
/// (テキスト選択 / Ctrl+wheel リサイズ / Ctrl+クリック URL を温存)。
/// 右/中/ホイール/Moved はジェスチャ判定せず即転送。
fn try_forward_mouse(app: &mut App, me: &MouseEvent, pane_rects: &HashMap<PaneId, Rect>) -> bool {
    use crate::mouse::MouseProtocolMode;
    use crossterm::event::{MouseButton, MouseEventKind::*};

    let mx = me.column as i32;
    let my = me.row as i32;

    // (1) ローカルドラッグ選択進行中: Drag/Up は既存ローカルアームへ委譲。
    if app.mouse_local_drag {
        if matches!(me.kind, Up(MouseButton::Left)) {
            app.mouse_local_drag = false;
        }
        return false;
    }

    // (flush) 保留クリックがあり続きが Drag(Left)/Up(Left) でない別イベント
    // なら、保留をクリックとして転送フラッシュしてから現イベントを処理する
    // (Down のまま Up を取り逃してクリックが消えるのを防ぐ)。
    let is_left_continuation = matches!(me.kind, Drag(MouseButton::Left) | Up(MouseButton::Left));
    if app.pending_mouse.is_some() && !is_left_continuation {
        flush_pending_click(app);
    }

    // (2) ペイン上か (タブバー/サイドバー/境界は None → ローカル)。
    let Some((pid, rect)) = find_pane_at(pane_rects, mx, my) else {
        return false;
    };
    // (3)(4) このペインの内側アプリのマウスモード/エンコーディング/alt 画面状態を取得。
    let Some((mode, enc, alt)) = app.panes.get(&pid).and_then(|p| {
        p.parser.lock().ok().map(|g| {
            (
                g.screen().mouse_protocol_mode(),
                g.screen().mouse_protocol_encoding(),
                g.screen().alternate_screen(),
            )
        })
    }) else {
        return false;
    };
    // (5) マウスモード None: 通常ペイン → 既存挙動を完全温存。
    if mode == MouseProtocolMode::None {
        return false;
    }
    // (6) ペインローカル座標 (既存クランプイディオムを踏襲)。
    let lx = (mx - rect.x).clamp(0, rect.w.saturating_sub(1)) as u16;
    let ly = (my - rect.y).clamp(0, rect.h.saturating_sub(1)) as u16;
    // (7) Shift / Ctrl は強制ローカル。
    if me
        .modifiers
        .intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL)
    {
        return false;
    }

    // (7.5) 縦ホイール: PRIMARY 画面ではローカル scrollback に温存。ALTERNATE 画面の
    // mouse-mode アプリ (Claude Code 等。alt grid は scrollback 容量 0 でローカル不可)
    // でのみ子へ SGR ホイールレポートとして転送する。None と Shift/Ctrl は上で除外済み。
    if mouse_event_reserved_for_local_scrollback(me)
        && !forward_vertical_wheel(mode, alt, me.modifiers)
    {
        return false;
    }

    // (8) ジェスチャ判定。
    match me.kind {
        Down(MouseButton::Left) => {
            // 即フォーカス + 保留 (press/drag 判定待ち)。何も転送しない。
            app.current_tab_mut().focused = pid;
            app.selection = None;
            app.sidebar_focused = false;
            app.pending_mouse = Some(crate::app::PendingMouse {
                pid,
                lx,
                ly,
                at: Instant::now(),
            });
            true
        }
        Drag(MouseButton::Left) => {
            if let Some(p) = app.pending_mouse {
                if p.pid == pid && (p.lx != lx || p.ly != ly) {
                    // 別セルへ移動 = ドラッグ選択確定。ローカルへ委譲する。
                    // 既存 Drag(Left) アームは selection 既存前提なのでここで生成。
                    app.pending_mouse = None;
                    app.mouse_local_drag = true;
                    app.current_tab_mut().focused = pid;
                    app.selection = Some(crate::app::Selection {
                        pane_id: pid,
                        anchor: (p.lx, p.ly as i32),
                        cursor: (lx, ly as i32),
                        dragging: true,
                        auto_scroll: 0,
                    });
                    app.last_left_click = Some((Instant::now(), pid, p.ly));
                    return false; // 同 Drag を既存アームで cursor 追従
                }
                // 同セル: まだクリックの可能性。保留継続。
                return true;
            }
            forward_or_swallow(app, pid, mode, enc, me, lx, ly)
        }
        Up(MouseButton::Left) => {
            if let Some(p) = app.pending_mouse.take() {
                // ドラッグ未発生 = クリック確定 → 合成 Down+Up を転送。
                app.current_tab_mut().focused = pid;
                app.selection = None;
                app.sidebar_focused = false;
                send_synth_click(app, pid, mode, enc, me.modifiers, p.lx, p.ly);
                return true;
            }
            forward_or_swallow(app, pid, mode, enc, me, lx, ly)
        }
        _ => {
            // 右/中ボタン・ホイール・Moved はジェスチャ判定せず即転送。
            forward_or_swallow(app, pid, mode, enc, me, lx, ly)
        }
    }
}

/// エンコードして PTY へ書く。`mode` の gating で報告対象外のときも、
/// アプリがマウスを掴んでいる以上ローカル UI を誤発火させないため
/// 飲み込む (常に `true`)。
fn forward_or_swallow(
    app: &mut App,
    pid: PaneId,
    mode: crate::mouse::MouseProtocolMode,
    enc: crate::mouse::MouseProtocolEncoding,
    me: &MouseEvent,
    lx: u16,
    ly: u16,
) -> bool {
    if let Some(bytes) = crate::mouse::encode_mouse_report(mode, enc, me, lx, ly) {
        if let Some(p) = app.panes.get(&pid) {
            p.write(&bytes);
        }
    }
    true
}

/// クリック確定時にアプリへ送る合成 press+release。`Press` モードでは
/// release レポートが None になるので press のみ送られる (X10 互換)。
fn send_synth_click(
    app: &mut App,
    pid: PaneId,
    mode: crate::mouse::MouseProtocolMode,
    enc: crate::mouse::MouseProtocolEncoding,
    modifiers: KeyModifiers,
    lx: u16,
    ly: u16,
) {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let down = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: lx,
        row: ly,
        modifiers,
    };
    let up = MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: lx,
        row: ly,
        modifiers,
    };
    let mut out = Vec::new();
    if let Some(b) = crate::mouse::encode_mouse_report(mode, enc, &down, lx, ly) {
        out.extend_from_slice(&b);
    }
    if let Some(b) = crate::mouse::encode_mouse_report(mode, enc, &up, lx, ly) {
        out.extend_from_slice(&b);
    }
    if !out.is_empty() {
        if let Some(p) = app.panes.get(&pid) {
            p.write(&out);
        }
    }
}

/// 保留中の Down を「クリック」として確定し転送する (Up を取り逃した時の保険)。
fn flush_pending_click(app: &mut App) {
    let Some(p) = app.pending_mouse.take() else {
        return;
    };
    let Some((mode, enc)) = app.panes.get(&p.pid).and_then(|pane| {
        pane.parser.lock().ok().map(|g| {
            (
                g.screen().mouse_protocol_mode(),
                g.screen().mouse_protocol_encoding(),
            )
        })
    }) else {
        return;
    };
    if mode == crate::mouse::MouseProtocolMode::None {
        return;
    }
    send_synth_click(app, p.pid, mode, enc, KeyModifiers::NONE, p.lx, p.ly);
}

fn find_pane_at(pane_rects: &HashMap<PaneId, Rect>, mx: i32, my: i32) -> Option<(PaneId, Rect)> {
    pane_rects
        .iter()
        .find(|(_, r)| mx >= r.x && mx < r.x + r.w && my >= r.y && my < r.y + r.h)
        .map(|(pid, r)| (*pid, *r))
}

/// クリック位置 (col,row) に重なる URL を vt100 セル列から検出して返す。
///
/// クリック行と前後の **ソフトラップ連鎖行** (vt100 の `row_wrapped` で連結
/// された行群) を 1 本の文字列として扱う。狭いペインで折り返された URL でも、
/// 連鎖全体を 1 つの URL として正しく取り出せる。
///
/// 検出規則: `http://` / `https://` をスキャンし、空白か制御文字に当たるまで
/// 取り込む。末尾の句読点 (`.` `,` `;` `:` `!` `?` `)` `]` `}` `>` `"` `'`)
/// は URL 外として削る。
fn url_at_cell(pane: &crate::pane::Pane, col: u16, row: u16) -> Option<String> {
    let parser = pane.parser.lock().ok()?;
    url_at_cell_in_screen(parser.screen(), col, row)
}

fn url_at_cell_in_screen(screen: &vt100::Screen, col: u16, row: u16) -> Option<String> {
    let (rows, cols) = screen.size();
    if row >= rows {
        return None;
    }

    // クリック行を含むソフトラップ連鎖を [chain_start, chain_end] で求める。
    // chain_start = row から後退して row_wrapped(prev) が true の間まで遡る。
    // chain_end   = row から前進して row_wrapped(cur)  が true の間まで進む。
    let mut chain_start = row;
    while chain_start > 0 && screen.row_wrapped(chain_start - 1) {
        chain_start -= 1;
    }
    let mut chain_end = row;
    while chain_end + 1 < rows && screen.row_wrapped(chain_end) {
        chain_end += 1;
    }

    // 連鎖行の文字列を (char, 表示列, 行) として展開。空セルは半角スペース扱い。
    // ワイド文字や合成は同じセルに複数 char が乗るため triple で持つ。
    let mut chars: Vec<(char, u16, u16)> = Vec::new();
    for r in chain_start..=chain_end {
        for x in 0..cols {
            let contents = screen.cell(r, x).map(|c| c.contents()).unwrap_or_default();
            if contents.is_empty() {
                chars.push((' ', x, r));
            } else {
                for ch in contents.chars() {
                    chars.push((ch, x, r));
                }
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
            // ヒット判定: URL を構成するいずれかのセルがクリック位置 (col,row)
            // と一致すれば、その URL を返す。連鎖行間も判定対象なので、
            // 折り返した URL の 2 行目をクリックしても拾える。
            let hit = chars[i..end]
                .iter()
                .any(|(_, c_col, c_row)| *c_col == col && *c_row == row);
            if hit {
                let url: String = chars[i..end].iter().map(|(c, _, _)| *c).collect();
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

    // (text, wrapped_to_next): wrapped_to_next が true の行は次行と
    // ソフトラップで連結しているので、コピー時に '\n' を挟まず、末尾の
    // trim_end も行わない (URL 末尾文字が削れないように)。
    let mut rows: Vec<(String, bool)> = Vec::new();
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
            // ソフトラップ判定。
            // 1) vt100 がこの行を wrapped と markしている (右端まで埋まり次行へ続く)。
            // 2) かつ、行末まで切れずに読み取れている (x1 が cols-1)。
            //    範囲末尾の `end` を含む行で x1 が短く切られている場合は
            //    URL 文字列としての連続性を保証できないので結合扱いにしない。
            let wrapped_to_next =
                screen.row_wrapped(y_u16) && upper == cols_now.saturating_sub(1) && our_row < end.1;
            rows.push((line, wrapped_to_next));
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

    // ソフトラップ行は改行・末尾trim無しで結合。それ以外は trim_end + '\n'。
    let mut out = String::new();
    let last_idx = rows.len().saturating_sub(1);
    for (i, (line, wrapped)) in rows.iter().enumerate() {
        let is_last = i == last_idx;
        if *wrapped {
            // 折り返し行: 末尾の URL/英数字が削れないよう trim せず連結し、改行も挟まない。
            out.push_str(line);
        } else {
            out.push_str(line.trim_end());
            if !is_last {
                out.push('\n');
            }
        }
    }
    out
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

/// ホイールとファントム矢印を対応付ける時間窓。Windows ConPTY は両者をほぼ
/// 同時に出すが、イベントループの tick (30ms) を跨いで別バッチに分離しうる。
/// 2 tick 強を確保し、別バッチ・逆順でも確実にコアレスする。実 Up/Down は
/// この窓だけ子への転送が遅延するが、履歴/メニュー操作では体感できない。
const PAIR_WINDOW: Duration = Duration::from_millis(70);

/// `ev` がホイール (ScrollUp/ScrollDown) の Mouse イベントか。
fn is_wheel_event(ev: &Event) -> bool {
    matches!(
        ev,
        Event::Mouse(me)
            if matches!(
                me.kind,
                crossterm::event::MouseEventKind::ScrollUp
                    | crossterm::event::MouseEventKind::ScrollDown
            )
    )
}

/// 子 PTY がマウスモードを有効化していても ccnest 側で処理するマウス入力。
fn mouse_event_reserved_for_local_scrollback(me: &MouseEvent) -> bool {
    matches!(
        me.kind,
        crossterm::event::MouseEventKind::ScrollUp | crossterm::event::MouseEventKind::ScrollDown
    )
}

/// 縦ホイールを子 PTY へ転送すべき (true) か ccnest ローカル scrollback に
/// 留める (false) かの純粋判定。ALTERNATE 画面の mouse-mode アプリ
/// (Claude Code 等。alt grid は scrollback 容量 0 でローカルスクロール不可) 上で、
/// Shift/Ctrl 修飾なしのときだけ転送する。`App` 非依存で table-test 可能。
fn forward_vertical_wheel(
    mode: crate::mouse::MouseProtocolMode,
    alt: bool,
    mods: KeyModifiers,
) -> bool {
    mode != crate::mouse::MouseProtocolMode::None
        && alt
        && !mods.intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL)
}

/// `ev` が修飾なしの Up/Down キー押下 (Press) か。ファントム矢印の候補判定に使う。
/// Release/Repeat や Shift/Ctrl/Alt 付きは候補にしない (それぞれ別用途)。
fn is_plain_updown_press(ev: &Event) -> bool {
    matches!(
        ev,
        Event::Key(k)
            if k.kind != KeyEventKind::Release
                && k.modifiers.is_empty()
                && matches!(k.code, KeyCode::Up | KeyCode::Down)
    )
}

/// バッチ `events` の位置 `idx` の plain Up/Down が、直前 or 直後の「非 paste」
/// 隣接イベントとしてホイールに挟まれているか。ホスト端末がホイールとファントム
/// 矢印を順不同・同一バッチで配信するケース (矢印 → ホイール含む) を、予算に
/// 依存せず確実に拾う。隣接判定は厳密に「最も近い非 paste イベント」のみを見る。
fn batch_adjacent_wheel(events: &[Event], idx: usize) -> bool {
    let nearest_before = events[..idx]
        .iter()
        .rev()
        .find(|e| !matches!(e, Event::Paste(_)));
    if nearest_before.is_some_and(is_wheel_event) {
        return true;
    }
    let nearest_after = events
        .get(idx + 1..)
        .and_then(|s| s.iter().find(|e| !matches!(e, Event::Paste(_))));
    nearest_after.is_some_and(is_wheel_event)
}

/// 非隣接で届いた plain Up/Down をどう扱うかの判定結果。
#[derive(Debug, PartialEq, Eq)]
enum ArrowAction {
    /// 確定ファントム。握りつぶす。
    Drop,
    /// 実キーかファントム先行か未確定。保留して対のホイールを待つ。
    Defer,
    /// 実キー確定。子へ転送する。
    Forward,
}

/// ホスト端末 (Windows ConPTY) がホイール回転を `\x1b[A`/`\x1b[B` に変換注入して
/// くる「ファントム矢印キー」を、決定論的に分類する純粋関数。`&App` に依存させず
/// bare field を受けることで `App::new` (内部で実 PTY を spawn) なしに単体テスト
/// 可能。副作用なし。
///
/// 判定:
/// - サイドバーフォーカス中 / rename 中 / 修飾付き / Up・Down 以外 → `Forward`
///   (サイドバーカーソル移動や Shift/Ctrl 矢印など実キーを絶対に握りつぶさない)
/// - `adjacent_wheel` = 同一バッチでホイールに隣接 → `Drop` (確定ファントム)
/// - 直前 `pair_window` 内にホイールあり → `Drop` (先行ホイールのファントム)
/// - それ以外 (ホイール未到来) → `Defer` (後続ホイールで Drop / 窓超過で Forward)
#[allow(clippy::too_many_arguments)]
fn classify_arrow(
    sidebar_focused: bool,
    renaming: bool,
    key: &KeyEvent,
    last_wheel_at: Option<Instant>,
    pair_window: Duration,
    now: Instant,
    adjacent_wheel: bool,
) -> ArrowAction {
    if sidebar_focused || renaming {
        return ArrowAction::Forward;
    }
    if !key.modifiers.is_empty() {
        return ArrowAction::Forward;
    }
    if !matches!(key.code, KeyCode::Up | KeyCode::Down) {
        return ArrowAction::Forward;
    }
    if adjacent_wheel {
        return ArrowAction::Drop;
    }
    let recent_wheel =
        matches!(last_wheel_at, Some(t) if now.saturating_duration_since(t) <= pair_window);
    if recent_wheel {
        return ArrowAction::Drop;
    }
    ArrowAction::Defer
}

/// `key` を子 PTY へ送るバイト列へ変換する。
///
/// `app_cursor` は子の DECCKM (アプリケーションカーソルキーモード, DECSET ?1)
/// 状態。true のとき矢印 / Home / End を CSI (`ESC [ X`) ではなく SS3
/// (`ESC O X`) で送る。Claude Code 等の TUI は DECCKM を有効化して SS3 形式を
/// 期待することがあり、CSI のまま送ると左右キーでカーソルが動かない。
/// false のときは従来どおり CSI 形式 (互換動作、回帰なし)。
fn key_to_bytes(key: &KeyEvent, app_cursor: bool) -> Vec<u8> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    // DECCKM: カーソル/編集キーの導入子を CSI(`\x1b[`) と SS3(`\x1bO`) で切替。
    let intro: &[u8] = if app_cursor { b"\x1bO" } else { b"\x1b[" };
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
        // 矢印 / Home / End は DECCKM に応じて導入子を切替 (CSI or SS3)。
        // 終端文字 (A/B/C/D/H/F) は両形式で共通。
        KeyCode::Left => {
            buf.extend_from_slice(intro);
            buf.push(b'D');
        }
        KeyCode::Right => {
            buf.extend_from_slice(intro);
            buf.push(b'C');
        }
        KeyCode::Up => {
            buf.extend_from_slice(intro);
            buf.push(b'A');
        }
        KeyCode::Down => {
            buf.extend_from_slice(intro);
            buf.push(b'B');
        }
        KeyCode::Home => {
            buf.extend_from_slice(intro);
            buf.push(b'H');
        }
        KeyCode::End => {
            buf.extend_from_slice(intro);
            buf.push(b'F');
        }
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
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};

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

    /// 狭い PTY (cols=20) で long URL を出すと、vt100 がソフトラップで
    /// 次行へ折り返す。コピー時にこの折り返しを '\n' で結合してしまうと
    /// URL が壊れる (実際のユーザ報告: %E2%94%82%E2%94%82 ≒ 罫線 / 改行混入)。
    /// row_wrapped() を見て、ソフトラップ行は改行を入れずに連結することを確認。
    #[test]
    fn extract_selected_text_joins_wrapped_url_without_newline() {
        let mut parser = vt100::Parser::new(5, 20, 100);
        // 20 列に収まらない長さの URL。先頭行 20 文字 + 折り返し 20 文字 + 残り。
        let url = "https://example.com/api/v1/long-path?token=abc123xyz&product=cli";
        parser.process(url.as_bytes());

        // URL が収まる行までを選択 (anchor=(0,0), cursor=(19, 3))。
        // 64 文字 / 20 cols = 行 0..3 に跨る。
        let sel = crate::app::Selection {
            pane_id: 1,
            anchor: (0, 0),
            cursor: (19, 3),
            dragging: false,
            auto_scroll: 0,
        };
        let text = extract_selected_text_from_parser(&mut parser, sel);
        // 改行が混入していないこと。URL がそのまま再構成されていること。
        assert!(
            !text.contains('\n'),
            "expected no newline in wrapped URL copy, got: {text:?}"
        );
        assert_eq!(text, url);
    }

    /// ハードラップ (明示的な \r\n) は従来どおり '\n' で結合される
    /// (ソフトラップ修正がハードラップを壊していないことの回帰テスト)。
    #[test]
    fn extract_selected_text_preserves_newline_for_hard_wrap() {
        let mut parser = vt100::Parser::new(5, 20, 100);
        parser.process(b"first\r\nsecond\r\nthird");
        let sel = crate::app::Selection {
            pane_id: 1,
            anchor: (0, 0),
            cursor: (19, 2),
            dragging: false,
            auto_scroll: 0,
        };
        let text = extract_selected_text_from_parser(&mut parser, sel);
        assert_eq!(text, "first\nsecond\nthird");
    }

    /// 折り返した URL のどの行をクリックしても、URL 全体が取れる。
    /// 1 行目クリック / 2 行目クリック / 3 行目クリックを順に検証。
    #[test]
    fn url_at_cell_detects_wrapped_url() {
        let mut parser = vt100::Parser::new(5, 20, 100);
        let url = "https://example.com/api/v1/long-path?x=1";
        parser.process(url.as_bytes());

        // 1 行目 (URL の先頭付近) をクリック。
        let got = url_at_cell_in_screen(parser.screen(), 5, 0);
        assert_eq!(got.as_deref(), Some(url));

        // 2 行目 (折り返し続き) をクリック。
        let got = url_at_cell_in_screen(parser.screen(), 3, 1);
        assert_eq!(got.as_deref(), Some(url));

        // URL の最終文字セル (url.len() = 40, cols = 20 → row 1, col 19) をクリック。
        let last_idx = url.len() - 1;
        let last_row = (last_idx / 20) as u16;
        let last_col = (last_idx % 20) as u16;
        let got = url_at_cell_in_screen(parser.screen(), last_col, last_row);
        assert_eq!(got.as_deref(), Some(url));
    }

    /// URL の外をクリックしたら None。連鎖行内でも URL の cell を踏んでなければ拾わない。
    #[test]
    fn url_at_cell_misses_when_outside_url() {
        let mut parser = vt100::Parser::new(5, 20, 100);
        parser.process(b"prefix https://example.com/x suffix");
        // 先頭 "prefix" の `p` (col=0, row=0) はヒットしない。
        let got = url_at_cell_in_screen(parser.screen(), 0, 0);
        assert!(got.is_none(), "got: {got:?}");
        // URL 範囲内 (col=10, row=0 付近) はヒット。
        let got = url_at_cell_in_screen(parser.screen(), 10, 0);
        assert!(got.is_some());
        assert!(got.as_deref().unwrap().starts_with("https://example.com"));
    }

    // --- 決定論的ファントム矢印コアレス (classify_arrow) --------------------
    //
    // Windows ConPTY が alt 画面中のホイール 1 回転を Mouse(ScrollUp/Down) と
    // plain な Up/Down KeyEvent の両方として (順不同・別バッチで) 配信するため、
    // 後者「ファントム矢印」を握りつぶす。タイミング窓 + 保留 (Defer) で
    // ホイール先行・ファントム先行・別バッチ・逆順のいずれもコアレスする。

    const W: Duration = Duration::from_millis(70);

    fn plain(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn wheel(kind: MouseEventKind) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        })
    }
    fn wheel_up() -> Event {
        wheel(MouseEventKind::ScrollUp)
    }
    fn wheel_down() -> Event {
        wheel(MouseEventKind::ScrollDown)
    }

    // -- classify_arrow --

    /// (a) 同一バッチで wheel→arrow に隣接 = 確定ファントム → Drop。
    #[test]
    fn classify_drops_when_adjacent_wheel() {
        let now = Instant::now();
        assert_eq!(
            classify_arrow(false, false, &plain(KeyCode::Up), None, W, now, true),
            ArrowAction::Drop
        );
    }

    /// (c) wheel が先行し別バッチで arrow が来た (window 内) = ファントム → Drop。
    #[test]
    fn classify_drops_when_recent_wheel_precedes() {
        let now = Instant::now();
        let recent = now - Duration::from_millis(40); // <= W
        assert_eq!(
            classify_arrow(
                false,
                false,
                &plain(KeyCode::Down),
                Some(recent),
                W,
                now,
                false
            ),
            ArrowAction::Drop
        );
    }

    /// (d) arrow が先行 (wheel 未到来) = 未確定 → Defer。これが旧予算方式で
    /// 漏れていたジェスチャ端のケース。保留してホイール到来 or 窓超過で確定する。
    #[test]
    fn classify_defers_when_no_wheel_yet() {
        let now = Instant::now();
        assert_eq!(
            classify_arrow(false, false, &plain(KeyCode::Up), None, W, now, false),
            ArrowAction::Defer
        );
    }

    /// 直近ホイールが window より古ければファントムではない → Defer (実キー候補)。
    #[test]
    fn classify_defers_when_wheel_too_old() {
        let now = Instant::now();
        let stale = now - Duration::from_millis(200); // > W
        assert_eq!(
            classify_arrow(
                false,
                false,
                &plain(KeyCode::Up),
                Some(stale),
                W,
                now,
                false
            ),
            ArrowAction::Defer
        );
    }

    /// (f) サイドバーフォーカス中は実キー扱い (カーソル移動を壊さない) → Forward。
    #[test]
    fn classify_forwards_when_sidebar_focused() {
        let now = Instant::now();
        assert_eq!(
            classify_arrow(true, false, &plain(KeyCode::Up), Some(now), W, now, true),
            ArrowAction::Forward
        );
    }

    /// (f) rename 中も実キー扱い → Forward。
    #[test]
    fn classify_forwards_when_renaming() {
        let now = Instant::now();
        assert_eq!(
            classify_arrow(false, true, &plain(KeyCode::Up), Some(now), W, now, true),
            ArrowAction::Forward
        );
    }

    /// 修飾付き矢印 (Ctrl+Up=Focus, Shift+Down=ScrollLine) は別用途 → Forward。
    #[test]
    fn classify_forwards_modified_arrows() {
        let now = Instant::now();
        let ctrl_up = KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL);
        let shift_down = KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT);
        assert_eq!(
            classify_arrow(false, false, &ctrl_up, Some(now), W, now, true),
            ArrowAction::Forward
        );
        assert_eq!(
            classify_arrow(false, false, &shift_down, Some(now), W, now, true),
            ArrowAction::Forward
        );
    }

    /// 左右キーはファントム対象外 → Forward。
    #[test]
    fn classify_forwards_left_right() {
        let now = Instant::now();
        assert_eq!(
            classify_arrow(false, false, &plain(KeyCode::Left), Some(now), W, now, true),
            ArrowAction::Forward
        );
        assert_eq!(
            classify_arrow(
                false,
                false,
                &plain(KeyCode::Right),
                Some(now),
                W,
                now,
                true
            ),
            ArrowAction::Forward
        );
    }

    // -- helper predicates --

    #[test]
    fn is_wheel_event_detects_scroll_only() {
        assert!(is_wheel_event(&wheel_up()));
        assert!(is_wheel_event(&wheel_down()));
        assert!(!is_wheel_event(&press(KeyCode::Up)));
    }

    #[test]
    fn is_plain_updown_press_classifies() {
        assert!(is_plain_updown_press(&press(KeyCode::Up)));
        assert!(is_plain_updown_press(&press(KeyCode::Down)));
        assert!(!is_plain_updown_press(&ctrl_press(KeyCode::Up)));
        assert!(!is_plain_updown_press(&press(KeyCode::Left)));
        assert!(!is_plain_updown_press(&release(KeyCode::Up)));
    }

    #[test]
    fn vertical_wheel_is_reserved_for_local_scrollback() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let scroll_up = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        };
        let scroll_down = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        };
        assert!(mouse_event_reserved_for_local_scrollback(&scroll_up));
        assert!(mouse_event_reserved_for_local_scrollback(&scroll_down));
    }

    #[test]
    fn forward_vertical_wheel_only_on_alt_mouse_mode() {
        use crate::mouse::MouseProtocolMode as M;
        let n = KeyModifiers::NONE;
        // alt + mouse-mode + 修飾なし → 子へ転送 (Claude Code が自前ビューをスクロール)。
        assert!(forward_vertical_wheel(M::AnyMotion, true, n));
        assert!(forward_vertical_wheel(M::Press, true, n)); // Press でもホイールは報告対象。
                                                            // プライマリ画面 → ローカル scrollback に温存。
        assert!(!forward_vertical_wheel(M::AnyMotion, false, n));
        // mouse-mode 無し (通常ペイン) → ローカル。
        assert!(!forward_vertical_wheel(M::None, true, n));
        // Shift / Ctrl は強制ローカル (ローカル強制 / 分割リサイズを温存)。
        assert!(!forward_vertical_wheel(
            M::AnyMotion,
            true,
            KeyModifiers::SHIFT
        ));
        assert!(!forward_vertical_wheel(
            M::AnyMotion,
            true,
            KeyModifiers::CONTROL
        ));
    }

    // -- batch_adjacent_wheel --

    #[test]
    fn batch_adjacent_wheel_prev_is_wheel() {
        let evs = vec![wheel_up(), press(KeyCode::Up)];
        assert!(batch_adjacent_wheel(&evs, 1));
    }

    #[test]
    fn batch_adjacent_wheel_next_is_wheel_reordered() {
        // ファントム矢印がホイールより先に届くケース。
        let evs = vec![press(KeyCode::Up), wheel_up()];
        assert!(batch_adjacent_wheel(&evs, 0));
    }

    #[test]
    fn batch_adjacent_wheel_lone_arrow_is_false() {
        let evs = vec![press(KeyCode::Up)];
        assert!(!batch_adjacent_wheel(&evs, 0));
    }

    #[test]
    fn batch_adjacent_wheel_non_immediate_is_false() {
        // 隣接は厳密に「最も近い非 paste イベント」。間に実キーが挟まれば false。
        let evs = vec![wheel_up(), press(KeyCode::Char('a')), press(KeyCode::Up)];
        assert!(!batch_adjacent_wheel(&evs, 2));
    }

    #[test]
    fn batch_adjacent_wheel_skips_paste() {
        let evs = vec![
            Event::Paste("x".to_string()),
            wheel_up(),
            press(KeyCode::Up),
        ];
        assert!(batch_adjacent_wheel(&evs, 2));
    }

    // -- input trace formatting (診断) --

    // --- key_to_bytes: DECCKM (アプリケーションカーソルキーモード) 切替 ------

    #[test]
    fn key_to_bytes_arrows_csi_in_normal_mode() {
        // app_cursor=false: 従来どおり CSI (ESC [ X)。
        assert_eq!(key_to_bytes(&plain(KeyCode::Left), false), b"\x1b[D");
        assert_eq!(key_to_bytes(&plain(KeyCode::Right), false), b"\x1b[C");
        assert_eq!(key_to_bytes(&plain(KeyCode::Up), false), b"\x1b[A");
        assert_eq!(key_to_bytes(&plain(KeyCode::Down), false), b"\x1b[B");
        assert_eq!(key_to_bytes(&plain(KeyCode::Home), false), b"\x1b[H");
        assert_eq!(key_to_bytes(&plain(KeyCode::End), false), b"\x1b[F");
    }

    #[test]
    fn key_to_bytes_arrows_ss3_in_application_mode() {
        // app_cursor=true: SS3 (ESC O X)。Claude Code 等が期待する形式。
        assert_eq!(key_to_bytes(&plain(KeyCode::Left), true), b"\x1bOD");
        assert_eq!(key_to_bytes(&plain(KeyCode::Right), true), b"\x1bOC");
        assert_eq!(key_to_bytes(&plain(KeyCode::Up), true), b"\x1bOA");
        assert_eq!(key_to_bytes(&plain(KeyCode::Down), true), b"\x1bOB");
        assert_eq!(key_to_bytes(&plain(KeyCode::Home), true), b"\x1bOH");
        assert_eq!(key_to_bytes(&plain(KeyCode::End), true), b"\x1bOF");
    }

    #[test]
    fn key_to_bytes_non_cursor_keys_unaffected_by_app_cursor() {
        // 文字・Enter・PageUp などは DECCKM の影響を受けない。
        assert_eq!(key_to_bytes(&plain(KeyCode::Char('a')), true), b"a");
        assert_eq!(key_to_bytes(&plain(KeyCode::Enter), true), b"\r");
        assert_eq!(key_to_bytes(&plain(KeyCode::PageUp), true), b"\x1b[5~");
        assert_eq!(key_to_bytes(&plain(KeyCode::PageUp), false), b"\x1b[5~");
    }

    #[test]
    fn format_trace_line_summarizes_batch() {
        let evs = vec![
            wheel_up(),
            press(KeyCode::Up),
            Event::Paste("hello".to_string()),
            Event::Resize(80, 24),
        ];
        let line = format_trace_line(&evs, true);
        assert!(line.starts_with("pending_arrow=true"), "got: {line}");
        assert!(line.contains("Mouse(ScrollUp@0,0)"), "got: {line}");
        assert!(line.contains("Key(Up,Press,"), "got: {line}");
        assert!(line.contains("Paste(len=5)"), "got: {line}");
        assert!(line.contains("Resize(80x24)"), "got: {line}");
    }
}

// Version History
// ver0.1 - 2026-05-21 - Honor vt100 row_wrapped() so soft-wrapped URLs survive
//                       drag+Ctrl+C copy and Ctrl+click open without stray newlines
//                       or border-char injection in narrow panes.
// ver0.2 - 2026-05-21 - Reserve vertical mouse wheel events for ccnest local
//                       scrollback/resize even when the child PTY requests mouse
//                       reporting, and keep phantom-arrow suppression alive across
//                       longer render stalls.
// ver0.3 - 2026-05-31 - Replace the racy wheel_budget/decay phantom-arrow
//                       heuristic with deterministic coalescing: a non-adjacent
//                       plain Up/Down is deferred (app.pending_arrow) until its
//                       paired wheel arrives (drop) or PAIR_WINDOW elapses
//                       (forward). Fixes wheel-up leaking as Claude prompt-history
//                       navigation at gesture edges (wheel/phantom split across
//                       batches or arriving out of order). Trace prefix is now
//                       pending_arrow=<bool>.
// ver0.4 - 2026-06-06 - Encode arrows / Home / End as SS3 (ESC O X) instead of CSI
//                       (ESC [ X) when the focused child has DECCKM (application
//                       cursor keys) enabled, so Left/Right actually move the
//                       cursor in apps like Claude Code. Normal mode keeps CSI
//                       (no regression).
