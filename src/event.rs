use std::collections::HashMap;
use std::sync::mpsc::RecvTimeoutError;
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
use crate::wake::{now_us, LoopMsg};

pub fn run_event_loop<B: Backend>(term: &mut Terminal<B>, mut app: App) -> Result<()> {
    // 何も起きないときの最大待ち。入力と PTY 出力は LoopMsg で即座に起こされる
    // ので、これはドラッグ auto-scroll / 2 秒 tick のための上限でしかない。
    // (かつては `event::poll(30ms)` のタイムアウトが「エコーが画面に出るまでの
    // 待ち」そのものだった: reader スレッドがループを起こす手段を持たず、
    // Windows のタイマー分解能 15.6ms で切り上げられて実質 31〜47ms 遅れていた。)
    let idle_wait = env_ms("CCNEST_IDLE_WAIT_MS", 30);
    let auto_scroll_interval = Duration::from_millis(30);
    // 出力駆動の再描画の最短間隔。ストリーミング中に 4KB チャンクごとに描かず、
    // 最初のチャンクは即描き、以降はこの間隔で束ねる。入力起因の変化と、
    // 打鍵直後の最初の出力 (= エコー) はこの制限を受けない。
    // CCNEST_MIN_OUTPUT_FRAME_MS で上書き可 (二分探索用)。
    let min_output_frame = env_ms("CCNEST_MIN_OUTPUT_FRAME_MS", 8);
    // 打鍵からこの時間以内に届いた最初の出力はエコーとみなして cap を免除する。
    let echo_window = env_ms("CCNEST_ECHO_WINDOW_MS", 100);
    let mut last_refresh = Instant::now();
    let mut last_auto_scroll = Instant::now();
    // タブのすべりを描き直す間隔 (約 60fps。Windows のタイマー分解能 15.6ms と同程度)。
    let tab_anim_interval = Duration::from_millis(16);
    let mut last_tab_anim = Instant::now();
    let refresh_every = Duration::from_secs(2);
    let mut pane_rects: HashMap<PaneId, Rect> = HashMap::new();
    let mut sidebar_file_rect: Option<Rect> = None;
    let mut tab_rects: Vec<(Rect, usize)> = Vec::new();
    // メニューの実描画矩形。draw が毎フレーム更新し、メニューを開いた直後は
    // event 側が None に無効化する (旧位置でのヒットテスト防止)。
    let mut menu_rect: Option<Rect> = None;
    // サイドバー表示遷移の検知用。非表示中は 2 秒 tick の git status walk を
    // 止めるため、表示された瞬間にここで 1 回だけ即時 refresh する。
    let mut sidebar_was_visible = app.sidebar.visible;

    let rx = app
        .take_wake_rx()
        .ok_or_else(|| anyhow::anyhow!("wake receiver already taken"))?;
    spawn_input_pump(app.wake_tx());
    // 利用制限 % の問い合わせ (別スレッド)。結果は 2 秒 tick で拾って描くので
    // ループを起こす必要はない。CCNEST_USAGE=off なら起動しない。
    crate::claude::usage::spawn_usage_poller(app.usage_cell.clone());

    // dirty: 次の周で描画が必要。input_dirty: その原因に入力/操作が含まれる
    // (frame cap を待たずに描く)。初回は必ず描く。
    // last_output_draw: 直近の「出力起因」の描画時刻。frame cap は出力起因の
    // 描画同士の間隔にだけ掛ける。打鍵直後の入力起因の描画を起点にすると、
    // その直後に届くエコーの描画が cap (タイマー分解能込みで最大 16ms) だけ
    // 遅れてしまう。
    let mut dirty = true;
    let mut input_dirty = true;
    // echo_exempt: この周の dirty は打鍵直後の出力 (エコー) を含む → cap を待たない。
    // input_dirty と違い、描画後の last_output_draw 更新は出力起因として行う
    // (ストリーミングの束ね方を変えない)。
    let mut echo_exempt = false;
    let mut last_output_draw = Instant::now() - min_output_frame;

    while !app.quit {
        // 選択の前提 (ペイン存在 / alt 画面状態) が崩れていたら描画前に破棄する。
        if validate_selection(&mut app) {
            dirty = true;
            input_dirty = true;
        }

        if dirty && (input_dirty || echo_exempt || last_output_draw.elapsed() >= min_output_frame) {
            term.draw(|f| {
                crate::ui::draw(
                    &app,
                    f,
                    &mut pane_rects,
                    &mut sidebar_file_rect,
                    &mut tab_rects,
                    &mut menu_rect,
                )
            })?;
            if latency_trace_enabled() {
                latency_trace_append(&format!(
                    "draw input_dirty={} echo_exempt={} pending_key={}",
                    input_dirty,
                    echo_exempt,
                    app.last_key_write_us.is_some()
                ));
            }
            if !input_dirty {
                last_output_draw = Instant::now();
            }
            dirty = false;
            input_dirty = false;
            echo_exempt = false;
            // 描画で確定したペイン矩形に PTY / parser のサイズを揃える。変わった
            // ペインがあれば parser は即座に新サイズになるので、次の周でもう一度
            // 描く (子の再描画は別途 Output 通知で追いかける)。
            if sync_pane_sizes(&mut app, &pane_rects) {
                dirty = true;
                input_dirty = true;
            }
            if latency_trace_enabled() {
                trace_latency_after_draw(&mut app);
            }
        }

        // ドラッグ端 auto-scroll: ペイン外側まで持っていかれたドラッグを 30ms 毎に
        // 1 行ずつ scroll する。anchor は絶対座標で内容に張り付いており、cursor を
        // ペイン端へ再ピンすることでスクロールバック内 (上端外) や未表示の最新行
        // (下端外) を 1 ドラッグで選択できる。
        if app
            .selection
            .as_ref()
            .is_some_and(|s| s.dragging && s.auto_scroll != 0)
            && last_auto_scroll.elapsed() >= auto_scroll_interval
        {
            advance_drag_auto_scroll(&mut app, &pane_rects);
            last_auto_scroll = Instant::now();
            dirty = true;
            input_dirty = true;
        }

        // タブのすべり (ドラッグで押しのけられた／離されて収まる): 途中のタブが
        // あるあいだだけ、約 16ms ごとに描き直す。終わったすべりはここで片付け、
        // 最後の 1 回で定位置に描いて止まる (以降は起こされるまで何もしない)。
        if app.has_tab_slides() && last_tab_anim.elapsed() >= tab_anim_interval {
            app.expire_tab_slides(Instant::now());
            last_tab_anim = Instant::now();
            dirty = true;
            input_dirty = true;
        }

        // 入力 or PTY 出力 or タイムアウトを待つ。出力起因で dirty のときは
        // frame cap の残りだけ待ち、それ以外はアイドル上限まで待つ。
        let wait = if dirty && !echo_exempt {
            min_output_frame.saturating_sub(last_output_draw.elapsed())
        } else if dirty {
            Duration::ZERO
        } else if app.has_tab_slides() {
            tab_anim_interval
                .saturating_sub(last_tab_anim.elapsed())
                .min(idle_wait)
        } else {
            idle_wait
        };
        let mut batch: Vec<Event> = Vec::new();
        let mut output_seen = false;
        match rx.recv_timeout(wait) {
            Ok(msg) => {
                absorb(&mut app, msg, &mut batch, &mut output_seen);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        // 同時に溜まっているメッセージを一気に drain して batch 化する。
        while let Ok(msg) = rx.try_recv() {
            absorb(&mut app, msg, &mut batch, &mut output_seen);
        }

        if !batch.is_empty() {
            // ペーストは Windows 上で Event::Paste ではなく個別 Key イベント群として
            // 届くことがあり、さらに ConPTY が 1 回の paste を複数チャンクに分割して
            // 届けることがある。バッチがペーストらしい (Press が 2 つ以上 /
            // Event::Paste あり) ときだけ、5ms 間隔で続きが届く限り最大 500ms まで
            // 集めて 1 batch に統合する。単発の打鍵はここで待たない (かつては
            // 全ての印字キーで 5ms = 実質 16ms 待っていた)。
            let burst_started = Instant::now();
            if should_extend_burst(&batch) {
                extend_paste_burst(&rx, &mut app, &mut batch, &mut output_seen, burst_started);
            }
            app.last_burst_wait_us = burst_started.elapsed().as_micros() as u64;
            let only_moves = batch_is_only_mouse_moves(&batch);
            process_batch(
                &mut app,
                batch,
                &pane_rects,
                sidebar_file_rect,
                &mut tab_rects,
                &mut menu_rect,
            )?;
            // 終了が確定した (Ctrl+Q / 最後のタブを閉じた) 直後はタブが 0 枚に
            // なり得る。以降の保留矢印フラッシュや tick は current_tab() を踏むので
            // ここで抜ける (非 quit 経路のフラッシュ位置は変えない)。
            if app.quit {
                break;
            }
            // ポインタ移動だけのバッチは ccnest の見た目を変えない (メニューの
            // ホバーとドラッグ中を除く) ので描かない。以前はマウスを動かすたびに
            // 即時フル描画になっていた。
            if !only_moves || moves_affect_ui(&app) {
                dirty = true;
                input_dirty = true;
            }
        }
        if output_seen {
            dirty = true;
            if app
                .echo_pending
                .take_if(|t| t.elapsed() <= echo_window)
                .is_some()
            {
                echo_exempt = true;
            } else {
                app.echo_pending = None;
            }
        }

        // 保留中の plain Up/Down フラッシュ: PAIR_WINDOW 内に対のホイールが
        // 来なければ実キー確定として子へ転送する。recv がタイムアウトでも
        // 毎周ここを通るので、入力が途絶えても保留が取り残されない。
        //
        // 必ず上の drain + process_batch の「直後」に置くこと (ループ先頭の
        // draw より前に停滞し得る処理を挟まない)。かつて draw の後・poll の前に
        // あったが、その配置だと 2 秒 tick (refresh_pane_state / git walk) 等で
        // 1 周が PAIR_WINDOW を超えた瞬間、キューに未読の対ホイールが残ったまま
        // ファントム矢印が実キー化されて子へ流れ、Claude のプロンプト履歴が
        // 開いてしまう (v0.1.7/0.1.8 の tick 重量化で再発した回帰)。ここなら
        // フラッシュ判定の直前に必ず drain が入り、先に処理されたホイールが
        // pending_arrow を相殺するため、tick がどれだけ停滞しても漏れない。
        if let Some((key, at)) = app.pending_arrow {
            if pending_arrow_expired(at, PAIR_WINDOW, Instant::now()) {
                app.pending_arrow = None;
                trace_pending_flush(&key);
                handle_key(&mut app, key, &pane_rects)?;
                dirty = true;
                input_dirty = true;
            }
        }

        // サイドバーが表示された瞬間は 2 秒 tick を待たず 1 回だけ即時 refresh
        // する (トグル箇所は複数あるため、ここで遷移検知に一本化)。
        if app.sidebar.visible && !sidebar_was_visible {
            app.sidebar.refresh();
            dirty = true;
        }
        sidebar_was_visible = app.sidebar.visible;

        if last_refresh.elapsed() >= refresh_every {
            // 非表示のサイドバーのために毎 tick フル git status walk
            // (untracked 再帰込み) + file tree 再走査を回さない。描画も操作も
            // 非表示中は sidebar データを読まないため据え置きで無害。
            if app.sidebar.visible {
                app.sidebar.refresh();
            }
            app.refresh_pane_state();
            // 緑が続いているタブを右へ小さく寄せ、🟨 に変わったタブを左へ戻す
            // (タブの位置が動くのはこの tick だけ)。
            app.refresh_tab_parking(Instant::now());
            // `ccnest --add` で外から届いた依頼を開き待ちに積む (下で 1 つ開く)。
            app.accept_remote_requests();
            // `--resume` の残りを 1 tick 1 タブずつ開く。失敗はステータス行に
            // 出して次へ (1 件の失敗で残りを止めない)。
            if let Err(e) = app.spawn_next_resume() {
                app.status = Some(format!("resume failed: {e}"));
            }
            last_refresh = Instant::now();
            dirty = true;
        }
    }
    Ok(())
}

/// crossterm の入力読み取りを専用スレッドへ逃がし、読めたイベントを
/// `LoopMsg::Input` としてイベントループへ送る。`event::read()` はコンソール
/// 入力ハンドルで無期限に待つので、ループ側はタイマーに頼らず「入力 or 出力」
/// を `recv_timeout` 一本で待てる。
///
/// 1 回の read ごとに `poll(0)` でキューに残っているぶんも drain して 1 通に
/// まとめる。人間の 1 打鍵はキューに 1 レコードしか無いので 1 イベントの
/// バッチになり、ペーストや IME 確定はまとめて積まれるので複数イベントの
/// バッチになる (`should_extend_burst` はこの差で判定する)。
///
/// 終了は考えない: メインが return → プロセス終了でこのスレッドも消える。
fn spawn_input_pump(tx: crate::wake::WakeTx) {
    std::thread::Builder::new()
        .name("ccnest-input".to_string())
        .spawn(move || loop {
            let first = match event::read() {
                Ok(ev) => ev,
                Err(_) => {
                    let _ = tx.send(LoopMsg::InputClosed);
                    return;
                }
            };
            let mut batch = vec![first];
            while matches!(event::poll(Duration::ZERO), Ok(true)) {
                match event::read() {
                    Ok(ev) => batch.push(ev),
                    Err(_) => break,
                }
            }
            if tx.send(LoopMsg::Input(batch)).is_err() {
                return;
            }
        })
        .expect("spawn ccnest-input thread");
}

/// 受信したメッセージをループ状態へ取り込む。入力なら `batch` に連結して true、
/// 出力通知なら (表示中のペインなら) `output_seen` を立てて false を返す。
fn absorb(app: &mut App, msg: LoopMsg, batch: &mut Vec<Event>, output_seen: &mut bool) -> bool {
    match msg {
        LoopMsg::Input(events) => {
            batch.extend(events);
            true
        }
        LoopMsg::Output(pid) => {
            if let Some(pane) = app.panes.get(&pid) {
                pane.waker.disarm();
            }
            let visible = app.pane_visible(pid);
            if visible {
                *output_seen = true;
            }
            if latency_trace_enabled() {
                latency_trace_append(&format!("output pid={pid} visible={visible}"));
            }
            false
        }
        LoopMsg::InputClosed => {
            // 以後キー入力が届かないので、固まった TUI を残さず終了する。
            app.quit = true;
            false
        }
    }
}

/// 純粋判定: drain 直後のバッチが「ペーストの可能性がある」か。
///
/// 人間の打鍵は 1 drain に Press 1 つ (Release は後から別バッチ) しか入らない
/// ので、paste 候補キー (Char/Enter/Tab、修飾なし) の Press が 2 つ以上並んで
/// いるか、bracketed-paste の `Event::Paste` を含むときだけ true。単発キーは
/// false = 即転送で、5ms (Windows 既定のタイマー分解能で実質 15.6ms) の
/// burst 待ちを払わない。IME 確定 (複数文字が同時に積まれる) は true になる
/// が、それは確定 1 回につき 1 度の待ちで、以前の「1 文字ごと」より軽い。
fn should_extend_burst(batch: &[Event]) -> bool {
    let mut presses = 0usize;
    for e in batch {
        match e {
            Event::Paste(_) => return true,
            Event::Key(k) if k.kind != KeyEventKind::Release && is_paste_candidate(e) => {
                presses += 1;
                if presses >= 2 {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// ペースト burst の続きを集める。直近の「入力」から 5ms 以内に次の入力が届く
/// 限り、`started` から最大 500ms まで `batch` に連結する。PTY 出力の通知は
/// 窓を延長しない (ペースト中は子がエコーを吐き続けるので、出力で延長すると
/// 500ms いっぱい待ってしまう)。
fn extend_paste_burst(
    rx: &crate::wake::WakeRx,
    app: &mut App,
    batch: &mut Vec<Event>,
    output_seen: &mut bool,
    started: Instant,
) {
    let gap = Duration::from_millis(5);
    let deadline = started + Duration::from_millis(500);
    let mut window_end = Instant::now() + gap;
    while batch.last().is_some_and(is_paste_candidate) {
        let now = Instant::now();
        let until = window_end.min(deadline);
        if now >= until {
            break;
        }
        match rx.recv_timeout(until - now) {
            Ok(msg) => {
                if absorb(app, msg, batch, output_seen) {
                    window_end = Instant::now() + gap;
                }
            }
            Err(_) => break,
        }
    }
}

/// `NAME` 環境変数を ms として読む。未設定・不正値・0 未満は `default_ms`。
/// 起動時に 1 回だけ評価する (ループ内で呼ばない)。
fn env_ms(name: &str, default_ms: u64) -> Duration {
    Duration::from_millis(parse_ms_override(
        std::env::var(name).ok().as_deref(),
        default_ms,
    ))
}

/// 純粋判定: 環境変数の値 (あれば) を ms として解釈する。空/非数/範囲外は既定値。
fn parse_ms_override(value: Option<&str>, default_ms: u64) -> u64 {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&v| v <= 10_000)
        .unwrap_or(default_ms)
}

/// 純粋判定: バッチがマウス移動 (ボタン無し) だけで構成されているか。
fn batch_is_only_mouse_moves(batch: &[Event]) -> bool {
    !batch.is_empty()
        && batch.iter().all(|e| {
            matches!(
                e,
                Event::Mouse(m) if m.kind == crossterm::event::MouseEventKind::Moved
            )
        })
}

/// ポインタ移動が ccnest の見た目に影響する状態か (メニューのホバー / ドラッグ選択中)。
fn moves_affect_ui(app: &App) -> bool {
    app.context_menu.is_some() || app.selection.as_ref().is_some_and(|s| s.dragging)
}

/// 描画で確定したペイン矩形 (`pane_rects` = 枠の内側) に PTY と parser の
/// サイズを揃える。変化があったペインが 1 つでもあれば true。
fn sync_pane_sizes(app: &mut App, pane_rects: &HashMap<PaneId, Rect>) -> bool {
    let mut changed = false;
    for (pid, r) in pane_rects {
        if let Some(pane) = app.panes.get_mut(pid) {
            changed |= pane.resize_if_changed(r.h.max(1) as u16, r.w.max(1) as u16);
        }
    }
    changed
}

/// 純粋判定: 保留中の plain Up/Down (Defer) を実キーとして確定フラッシュすべきか。
/// classify_arrow の Drop 窓が `<= pair_window` なので、こちらは厳密不等号で
/// 相補させ境界の隙間を作らない。`saturating_duration_since` により
/// now < deferred_at (計測順序の逆転) でも panic せず false を返す。
fn pending_arrow_expired(deferred_at: Instant, pair_window: Duration, now: Instant) -> bool {
    now.saturating_duration_since(deferred_at) > pair_window
}

/// 単一イベントを診断ログ向けに 1 トークンへ整形する純粋関数。
fn fmt_event(ev: &Event) -> String {
    match ev {
        Event::Key(k) => format!("Key({:?},{:?},{:?})", k.code, k.kind, k.modifiers),
        // 修飾キーは付いているときだけ出す (`Mouse(Down(Left)@10,5,KeyModifiers(ALT))`)。
        // ホスト端末が Alt / Shift 付きのクリックを渡してくるかの実測に使う。
        Event::Mouse(m) if !m.modifiers.is_empty() => format!(
            "Mouse({:?}@{},{},{:?})",
            m.kind, m.column, m.row, m.modifiers
        ),
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

/// 入力トレースが有効なときだけ、タイムスタンプ付き 1 行を
/// `%APPDATA%\ccnest\input-trace.log` (OS データディレクトリ配下) に追記する。
/// 既定では即 return し挙動・性能に一切影響しない。全 IO は best-effort で
/// エラー無視 (診断が本体を壊さない)。
fn trace_append_line(line: &str) {
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
        let _ = writeln!(f, "{} {}", ts, line);
    }
}

/// 1 バッチ分の入力イベント列をトレースログへ追記する。
fn trace_input_batch(events: &[Event], pending_arrow: bool) {
    if !input_trace_enabled() {
        return;
    }
    trace_append_line(&format_trace_line(events, pending_arrow));
}

/// 保留矢印の実キー確定フラッシュを `pending_arrow_flush Key(...)` として記録
/// する。ホイール操作だけのセッションでこの行が出たら、ファントム矢印が実キー
/// として子へ漏れた署名 (E2E 検証は Select-String "pending_arrow_flush" が
/// 0 件であることを確認する)。
fn trace_pending_flush(key: &KeyEvent) {
    if !input_trace_enabled() {
        return;
    }
    trace_append_line(&format!(
        "pending_arrow_flush {}",
        fmt_event(&Event::Key(*key))
    ));
}

/// `CCNEST_LATENCY_TRACE` 環境変数を初回だけ評価。未設定なら以降ゼロコスト。
fn latency_trace_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("CCNEST_LATENCY_TRACE").is_ok())
}

/// 遅延トレース 1 行を `%APPDATA%\ccnest\latency-trace.log` に追記する。
/// `trace_append_line` と同じ best-effort (エラー無視・本体を壊さない)。
fn latency_trace_append(line: &str) {
    let Some(base) = dirs::data_dir() else {
        return;
    };
    let dir = base.join("ccnest");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("latency-trace.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write as _;
        let ts = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f");
        let _ = writeln!(f, "{} {}", ts, line);
    }
}

/// 1 打鍵ぶんの遅延内訳を 1 行に整形する純粋関数 (単体テスト可能)。
/// 単位はマイクロ秒で受け取り、ms 小数 1 桁で出す。
///
/// - `key_write_to_output`: キーを子 PTY へ書いてから、フォーカスペインの
///   parser に次の出力が反映されるまで (子の応答 + ConPTY 往復)
/// - `output_to_draw`: その出力が反映されてから描画が完了するまで
///   (= ccnest 自身の表示遅延)
/// - `burst_wait`: そのキーのバッチで費やしたペースト判定待ち
fn format_latency_line(
    key_write_to_output_us: u64,
    output_to_draw_us: u64,
    burst_wait_us: u64,
) -> String {
    format!(
        "latency key_write->output={:.1}ms output->draw={:.1}ms burst_wait={:.1}ms total={:.1}ms",
        key_write_to_output_us as f64 / 1000.0,
        output_to_draw_us as f64 / 1000.0,
        burst_wait_us as f64 / 1000.0,
        (key_write_to_output_us + output_to_draw_us + burst_wait_us) as f64 / 1000.0,
    )
}

/// 描画直後に呼ぶ。直近のキー書き込み後にフォーカスペインへ出力が反映されて
/// いれば、その打鍵の遅延内訳を記録して計測をクリアする。まだ出力が来て
/// いなければ (子が応答中) 何もせず次の描画で再判定する。
///
/// 「キー書き込み後に最初に来た出力」をエコーとみなす近似。スピナー等の
/// 無関係な出力が先に来ると短めに出るので、計測はアイドルなプロンプトで行う。
fn trace_latency_after_draw(app: &mut App) {
    let Some(write_us) = app.last_key_write_us else {
        return;
    };
    let focused = app.current_tab().focused;
    let Some(pane) = app.panes.get(&focused) else {
        app.last_key_write_us = None;
        return;
    };
    let out_us = pane.output_stamp.last_us();
    if out_us < write_us {
        return;
    }
    let draw_us = now_us();
    latency_trace_append(&format_latency_line(
        out_us - write_us,
        draw_us.saturating_sub(out_us),
        app.last_burst_wait_us,
    ));
    app.last_key_write_us = None;
    app.last_burst_wait_us = 0;
}

fn process_batch(
    app: &mut App,
    events: Vec<Event>,
    pane_rects: &HashMap<PaneId, Rect>,
    sidebar_file_rect: Option<Rect>,
    tab_rects: &mut Vec<(Rect, usize)>,
    menu_rect: &mut Option<Rect>,
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
        // quit 確定後 (Ctrl+Q / 最後のタブを閉じた) はタブが 0 枚になり得るので、
        // 同一バッチの残りイベントは処理しない (current_tab() の範囲外 panic 予防)。
        if app.quit {
            break;
        }
        // 連続する paste 系イベント (Event::Paste と classify_run でマッチする
        // Char/Enter/Tab run) をまとめて 1 回の handle_paste にする。
        // Windows ConPTY が大きいペーストを複数チャンク=複数 Event::Paste で
        // 配信するため、ここで合流させないと Claude 側で N 個の placeholder に
        // 分裂して見える。
        if let Some((consumed, text)) = collect_paste_segment(&events[i..]) {
            // コンテキストメニューは「その他の入力で閉じる」ルールに合わせ、
            // ターミナル側ペーストでも閉じてからフォーカス先へ流す
            // (開いたまま裏へ書き込まれるのを防ぐ)。
            app.context_menu = None;
            // 名前の入力中は、貼り付けも名前の欄へ (裏のペインへ流さない)。
            if app.renaming_tab.is_some() {
                paste_into_rename(app, &text);
            } else {
                handle_paste(app, &text);
            }
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
                        app.renaming_tab.is_some() || app.context_menu.is_some(),
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
                handle_mouse(
                    app,
                    *me,
                    pane_rects,
                    sidebar_file_rect,
                    tab_rects,
                    menu_rect,
                );
            }
            Event::Resize(..) => {
                // ホストリサイズは全ペインの再フロー (行の折り返し直し) を引き起こし、
                // 選択の座標が指す内容が変わるため破棄する。メニューもアンカーが
                // 新しい端末サイズの外を指し得るため閉じる。
                app.selection = None;
                app.context_menu = None;
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
    // コンテキストメニューモーダル: rename より先・選択クリア (下の Ctrl+C 分岐)
    // より先に全キーを横取りする。メニュー操作で選択が消えないための順序。
    if app.context_menu.is_some() {
        handle_context_menu_key(app, key);
        return Ok(());
    }

    // Rename mode: intercept everything before keymap resolution.
    if app.renaming_tab.is_some() {
        handle_rename_key(app, key);
        return Ok(());
    }

    // 選択範囲が存在する状態で Ctrl+C → クリップボードへコピーして選択解除。
    // reshell / pass-through より優先。その他のキーは選択をクリアしてから通常処理。
    // グリッド識別 (alt / reset 世代) のガードは extract 側が抽出と同一ロック下で
    // 行う (別ロックでの事前チェックは reader スレッドと TOCTOU になるため)。
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
        Action::ReopenClosedTab => {
            if !app.reopen_closed_tab()? {
                app.status = Some("開き直せる閉じたタブはありません".to_string());
            }
            app.selection = None;
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
            app.begin_rename_tab();
        }
        Action::ToggleHoldTab => {
            let idx = app.active_tab;
            if let Some(held) = app.toggle_hold_tab(idx) {
                // 前面が隣のタブへ移るので、前のタブの選択は残さない。
                app.selection = None;
                if input_trace_enabled() {
                    trace_append_line(&format!(
                        "hold_tab idx={idx} held={held} active={}",
                        app.active_tab
                    ));
                }
            }
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
        Action::PasteClipboard => {
            if app.sidebar_focused {
                return Ok(());
            }
            let focused_id = app.current_tab().focused;
            paste_clipboard_to_pane(app, focused_id);
            app.selection = None;
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
                    app.echo_pending = Some(Instant::now());
                    if latency_trace_enabled() {
                        app.last_key_write_us = Some(now_us());
                    }
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
    // Ctrl+V: クリップボードの文字を名前の欄へ (ペインには流さない)。
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('v') | KeyCode::Char('V'))
    {
        let text = arboard::Clipboard::new()
            .ok()
            .and_then(|mut cb| cb.get_text().ok());
        if let Some(t) = text {
            paste_into_rename(app, &t);
        }
        return;
    }
    let mut fresh = app.rename_fresh;
    let Some(buf) = app.renaming_tab.as_mut() else {
        return;
    };
    let outcome = rename_key(buf, &mut fresh, &key);
    app.rename_fresh = fresh;
    apply_rename_outcome(app, outcome);
}

/// コンテキストメニュー開時のキー入力 (handle_key 最上部から呼ばれるモーダル)。
/// Esc = 閉じる / ↑↓ = enabled 項目間を移動 (wrap) / Enter = 実行。
/// その他のキーは閉じて飲む (メニューの下の子プロセスへ漏らさない)。
fn handle_context_menu_key(app: &mut App, key: KeyEvent) {
    let Some(menu) = app.context_menu.clone() else {
        return;
    };
    match key.code {
        KeyCode::Esc => app.context_menu = None,
        KeyCode::Up | KeyCode::Down => {
            let dir = if matches!(key.code, KeyCode::Up) {
                -1
            } else {
                1
            };
            let next = next_enabled(&menu.items, menu.highlighted, dir);
            if let Some(m) = app.context_menu.as_mut() {
                m.highlighted = next;
            }
        }
        KeyCode::Enter => {
            // 無効項目上の Enter は no-op (メニューは開いたまま)。
            if menu.items.get(menu.highlighted).is_some_and(|i| i.enabled) {
                execute_menu_action(app, menu.items[menu.highlighted].action, menu.pane_id);
            }
        }
        _ => app.context_menu = None,
    }
}

/// items の `from` から `dir` (±1) 方向で最も近い enabled 項目 index (wrap)。
/// enabled が 1 つも無ければ `from` をそのまま返す。純粋関数。
fn next_enabled(items: &[crate::app::MenuItem], from: usize, dir: i32) -> usize {
    if items.is_empty() {
        return from;
    }
    let n = items.len() as i32;
    let mut i = from as i32;
    for _ in 0..items.len() {
        i = (i + dir).rem_euclid(n);
        if items[i as usize].enabled {
            return i as usize;
        }
    }
    from
}

/// 最初の enabled 項目 index。純粋関数。
fn first_enabled(items: &[crate::app::MenuItem]) -> Option<usize> {
    items.iter().position(|i| i.enabled)
}

fn scroll_focused(app: &mut App, delta: i32) {
    let focused_id = app.current_tab().focused;
    scroll_pane_with_selection(app, focused_id, delta);
}

/// `pane.scroll_by` を呼ぶ。選択は絶対座標なので非ドラッグ時の補正は不要
/// (反転帯は構造的に内容へ張り付く)。ドラッグ中のみ、マウス直下に cursor を
/// 留めるため viewport 先頭の移動量ぶん cursor を平行移動する
/// (= 反転がホイール方向の新しい行を取り込んで伸びる。ブラウザの選択と同じ)。
fn scroll_pane_with_selection(app: &mut App, pane_id: PaneId, delta: i32) {
    let Some(pane) = app.panes.get(&pane_id) else {
        return;
    };
    let top_before = pane.top_abs();
    pane.scroll_by(delta);
    let shift = pane.top_abs() - top_before;
    if shift == 0 {
        return;
    }
    if let Some(sel) = app.selection.as_mut() {
        if sel.pane_id == pane_id && sel.dragging {
            sel.cursor.1 += shift;
        }
    }
}

/// (viewport 先頭の abs 行, alternate screen 中か, reset 世代) を 1 回のロックで
/// 取得する。選択の作成・更新時に画面座標 → 絶対座標へ変換するために使う。
fn pane_view_state(app: &App, pid: PaneId) -> Option<(i64, bool, u64)> {
    let pane = app.panes.get(&pid)?;
    let parser = pane.parser.lock().ok()?;
    let s = parser.screen();
    Some((
        crate::pane::viewport_top_abs(s),
        s.alternate_screen(),
        s.reset_generation(),
    ))
}

/// ドラッグ中の auto-scroll を 1 行進める。
///
/// `selection.auto_scroll` の符号:
/// - `+1` (下端外) → 新しい行を見たい → vt100 の scrollback offset を `-1`。
/// - `-1` (上端外) → 古い行を見たい → vt100 の scrollback offset を `+1`。
///
/// anchor は絶対座標で内容に張り付いているため補正不要。cursor はペイン端に
/// 張り付き続ける必要があるため、スクロール後の viewport 先頭から毎 tick
/// 端の abs 行を計算し直してピンする。scrollback がクランプされて動かなくても、
/// ストリーミング出力で viewport が動いていれば追従する。
fn advance_drag_auto_scroll(app: &mut App, pane_rects: &HashMap<PaneId, Rect>) {
    let Some(sel) = app.selection else {
        return;
    };
    if !sel.dragging || sel.auto_scroll == 0 {
        return;
    }
    let scroll_delta = -sel.auto_scroll;
    let Some(pane) = app.panes.get(&sel.pane_id) else {
        return;
    };
    pane.scroll_by(scroll_delta);
    let top = pane.top_abs();
    let h = pane_rects
        .get(&sel.pane_id)
        .map(|r| r.h.max(1) as i64)
        .unwrap_or(24);
    let pinned = if sel.auto_scroll < 0 {
        top
    } else {
        top + h - 1
    };
    if let Some(s) = app.selection.as_mut() {
        s.cursor.1 = pinned;
    }
}

/// 選択の前提が崩れていたら破棄する。ペイン消滅、子アプリの alternate screen
/// 状態が選択作成時と食い違った (グリッドが入れ替わり、abs 座標が別の内容を
/// 指してしまう)、または RIS (ESC c) でグリッドが作り直された (reset 世代の
/// 不一致) ケース。選択が存在するときだけロックを取る。
/// 破棄したら true (再描画が要る)。
fn validate_selection(app: &mut App) -> bool {
    let Some(sel) = app.selection else {
        return false;
    };
    let ok = app
        .panes
        .get(&sel.pane_id)
        .and_then(|p| {
            p.parser.lock().ok().map(|pr| {
                let s = pr.screen();
                s.alternate_screen() == sel.alt && s.reset_generation() == sel.grid_gen
            })
        })
        .unwrap_or(false);
    if !ok {
        app.selection = None;
    }
    !ok
}

fn handle_mouse(
    app: &mut App,
    me: MouseEvent,
    pane_rects: &HashMap<PaneId, Rect>,
    sidebar_file_rect: Option<Rect>,
    tab_rects: &mut Vec<(Rect, usize)>,
    menu_rect: &mut Option<Rect>,
) {
    use crossterm::event::{MouseButton, MouseEventKind::*};
    let mx = me.column as i32;
    let my = me.row as i32;

    // (0) コンテキストメニューモーダル: 開いている間は全マウスイベントを横取り。
    // try_forward より先に判定し、子 (マウスモードのアプリ) へ一切漏らさない。
    if let Some(menu) = app.context_menu.clone() {
        let (inside, over) = menu_hit(*menu_rect, menu.items.len(), mx, my);
        match classify_menu_mouse(&me.kind, inside, over, &menu.items) {
            MenuMouseAction::Highlight(i) => {
                if let Some(m) = app.context_menu.as_mut() {
                    m.highlighted = i;
                }
                return;
            }
            MenuMouseAction::Execute(i) => {
                execute_menu_action(app, menu.items[i].action, menu.pane_id);
                // 対の Up(Left) が press なしの release として子へ転送されない
                // よう立てる (mouse_local_drag の第 2 用途。Up はローカルで消化)。
                app.mouse_local_drag = true;
                return;
            }
            MenuMouseAction::Consume => return,
            MenuMouseAction::Close => {
                app.context_menu = None;
                if matches!(me.kind, Down(MouseButton::Left)) {
                    app.mouse_local_drag = true;
                }
                // 外側クリック/ホイールは「メニューを閉じる」操作として飲み、
                // 下のペインには落とさない。
                return;
            }
            MenuMouseAction::Reopen => {
                // 閉じて下の (0.5) で新しい位置に開き直す。
                app.context_menu = None;
            }
        }
    }

    // (0.3) タブを引いている最中 (タブ上の Down(Left) で開始): Drag(Left) は
    // 並べ替え、Up(Left) で終わり。try_forward より先に取るのは、ポインタが
    // ペインの上まで下がったとき、press の無い Drag/Up がマウスモードの子へ
    // 流れないようにするため。縦位置は見ない (横位置だけで判定)。
    if let Some(mut drag) = app.tab_drag {
        match me.kind {
            Drag(MouseButton::Left) => {
                // 押した桁から動いたら浮かせる (描画がポインタの位置に重ねて描く)。
                // クリックの手ぶれ (同じ桁の Drag) では浮かせない。
                drag.pointer_x = mx;
                if mx != drag.press_x && crate::app::tab_anim_enabled() {
                    drag.floating = true;
                }
                let from = drag.idx;
                // 動かせるのは同じ組の中だけ (名前つきの組 / 右へ小さく寄せた組 /
                // 保留の組)。組は状態と保留で決まるので、引いても組は越えられない。
                // 組の中ではタブバーの並び = タブ番号の順なので、判定は今までと同じ。
                let group = same_group_rects(tab_rects, from, |i| {
                    app.tabs.get(i).map(crate::app::Tab::group)
                });
                if let Some(to) = tab_drop_target(&group, from, mx) {
                    // 押しのけられるタブが動く桁数 = 引いているタブの幅 + 区切り 1。
                    let shift = tab_rects
                        .iter()
                        .find(|(_, i)| *i == from)
                        .map_or(0, |(r, _)| r.w + 1);
                    if app.move_tab(from, to) {
                        if drag.floating {
                            app.slide_displaced(from, to, shift, Instant::now());
                            // 間にいた別の組のタブは、タブバーの上では動いていない。
                            app.cancel_slides_outside_group(to);
                        }
                        drag.idx = to;
                        if input_trace_enabled() {
                            trace_append_line(&format!("move_tab from={from} to={to}"));
                        }
                        // 動かした瞬間に描画済みの矩形は古くなる。次フレームで
                        // draw_tabbar が組み直すまで判定に使わせない (同じバッチの
                        // 続きの Drag が古い矩形で二重に動かさないように)。
                        tab_rects.clear();
                    }
                }
                app.tab_drag = Some(drag);
                return;
            }
            Up(MouseButton::Left) => {
                // 浮いていた位置から、空けてあった定位置へすべらせて収める。
                // (動かした直後で矩形が無いフレームは、すべらせず定位置に出す。)
                if drag.floating {
                    if let Some((fx, slot_x)) = crate::app::float_in_strip(tab_rects, &drag) {
                        app.settle_tab(drag.idx, fx - slot_x, Instant::now());
                    }
                }
                app.tab_drag = None;
                return;
            }
            // 別のボタン・ホイールなど: 並べ替えを打ち切って通常の処理へ。
            _ => app.tab_drag = None,
        }
    }

    // (0.5) 右クリック → ローカルのコンテキストメニューを開く。マウスモードの
    // 子への転送 (try_forward) より先に横取りする。従来は Claude Code ペインで
    // 右クリックが子へ転送されて何も起きなかった。
    if matches!(me.kind, Down(MouseButton::Right)) {
        // 保留中の左クリックが残っていたら先にクリックとして確定させる。
        flush_pending_click(app);
        if let Some((pid, _)) = find_pane_at(pane_rects, mx, my) {
            if right_click_is_local(app, pid) {
                // 進行中の左ドラッグはここで確定させる。メニューが対の Up(Left) を
                // 飲むため、放置すると dragging/auto_scroll が取り残されて
                // auto-scroll が回り続け、mouse_local_drag も残って以降の
                // マウスルーティングが狂う (左右チョード押しのケース)。
                if let Some(sel) = app.selection.as_mut() {
                    sel.dragging = false;
                    sel.auto_scroll = 0;
                    if sel.anchor == sel.cursor {
                        app.selection = None;
                    }
                }
                app.mouse_local_drag = false;
                open_context_menu(app, pid, mx, my);
                // 直前フレームの矩形は旧メニュー (または無し) のもの。新メニューが
                // 描画されるまでヒットテストに使わせない (同一バッチ内の連続
                // イベントが旧位置の項目を誤実行しないように)。
                *menu_rect = None;
                return;
            }
        } else {
            // ペイン外 (タブバー/サイドバー/ステータスバー) の右クリックは無視。
            return;
        }
    }

    // (0.7) 名前の入力中の左クリック: 入力中のタブの外なら確定 (Explorer と同じ。
    // Enter と同じ決まり = 何も打っていなければ何も変えない)、入力中のタブの上
    // なら入力を続ける。どちらもクリックは飲む: マウスモードの子へ流さず
    // (try_forward より先)、タブの切替や選択も始めない。対の Up はローカルで消化。
    if app.renaming_tab.is_some() && matches!(me.kind, Down(MouseButton::Left)) {
        flush_pending_click(app);
        if tab_at(tab_rects, mx, my) != Some(app.active_tab) {
            let outcome =
                rename_finish(app.renaming_tab.as_deref().unwrap_or(""), app.rename_fresh);
            apply_rename_outcome(app, outcome);
        }
        app.mouse_local_drag = true;
        return;
    }

    // フォーカス先ペインの内側アプリがマウス報告を要求しているなら、
    // crossterm の MouseEvent をそのアプリ向けレポートに変換して PTY へ
    // 転送する。転送した (or アプリが掴んでいて飲み込んだ) ら以降の
    // ccnest 内部 UI 処理 (選択/スクロール/タブ等) はスキップする。
    if try_forward_mouse(app, &me, pane_rects) {
        return;
    }

    // タブバー: 左クリック = 切替、左ダブルクリック = 名前の入力、中クリック
    // (ホイールクリック) = そのタブを配下の全ペインごと閉じる (ブラウザ流・確認
    // なし)。try_forward の後に置くのは、非左イベント到着時の保留クリック flush を
    // 従来どおり先に済ませるため (タブバー座標はペイン外なので子へは転送されない)。
    // リネーム中はどれも無視 (左 Down は上の (0.7) が先に取る)。
    let tab_hit = tab_at(tab_rects, mx, my);
    let now = Instant::now();
    let double = is_tab_double_click(app.last_tab_click, now, tab_hit);
    match classify_tab_mouse(&me.kind, tab_hit, app.renaming_tab.is_some(), double) {
        TabMouseAction::Rename(i) if i < app.tabs.len() => {
            // 1 回目の押下でもう前面になっている。並べ替えは始めず (tab_drag を
            // 立てない)、対の Up / 手ぶれの Drag はローカルで消化する。
            app.active_tab = i;
            app.selection = None;
            app.tab_drag = None;
            app.last_tab_click = None;
            app.begin_rename_tab();
            app.mouse_local_drag = true;
            if input_trace_enabled() {
                trace_append_line(&format!("begin_rename_tab idx={i}"));
            }
            return;
        }
        TabMouseAction::Switch(i) if i < app.tabs.len() => {
            app.active_tab = i;
            app.selection = None;
            // 500ms 以内に同じタブをもう一度押したら名前の入力 (上の Rename)。
            app.last_tab_click = Some((now, i));
            // このまま左右に引けば並べ替え (上の (0.3))。離せばただの切替。
            let tab_x = tab_rects
                .iter()
                .find(|(_, idx)| *idx == i)
                .map_or(mx, |(r, _)| r.x);
            app.tab_drag = Some(crate::app::TabDrag {
                idx: i,
                press_x: mx,
                grab_dx: mx - tab_x,
                pointer_x: mx,
                floating: false,
            });
            return;
        }
        TabMouseAction::Close(i) => {
            // 範囲外 idx (閉じた直後の古い矩形) は App 側で no-op。
            app.close_tab(i);
            if input_trace_enabled() {
                trace_append_line(&format!("close_tab idx={i} tabs_left={}", app.tabs.len()));
            }
            app.selection = None;
            // 描画済みの矩形は閉じた瞬間に古くなる。次フレームで draw_tabbar が
            // 組み直すまでヒットテストに使わせない (menu_rect = None と同じ考え方)。
            tab_rects.clear();
            return;
        }
        TabMouseAction::Switch(_) | TabMouseAction::Rename(_) | TabMouseAction::Ignore => {}
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
                // ペインリサイズは pty 再フローで行の折り返しが変わり、選択の
                // 座標が指す内容とズレるため破棄する。
                app.selection = None;
            } else {
                let delta = if matches!(me.kind, ScrollUp) { 1 } else { -1 };
                scroll_pane_with_selection(app, target, delta * 3);
            }
        }
        Down(MouseButton::Left) => {
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
                // Alt / Shift + クリック: 残っている選択 (無ければ直前の普通の
                // クリック位置) からここまでを選ぶ。伸ばせる始点が無ければ下の
                // 普通のクリックへ落ちる。
                if is_extend_click(me.modifiers) {
                    let extended = pane_view_state(app, pid).and_then(|(top, alt, grid_gen)| {
                        extended_selection(
                            app.selection,
                            app.click_anchor,
                            pid,
                            (lx, top + ly as i64),
                            alt,
                            grid_gen,
                            (rect.w, rect.h),
                        )
                    });
                    if let Some(sel) = extended {
                        if input_trace_enabled() {
                            trace_append_line(&format!(
                                "extend_selection pane={pid} anchor={:?} cursor={:?}",
                                sel.anchor, sel.cursor
                            ));
                        }
                        app.selection = Some(sel);
                        // マウスモードのペインでも、続きの Drag / Up を子へ流さず
                        // ローカルの選択アームで受ける (Alt を先に離しても同じ)。
                        app.mouse_local_drag = true;
                        app.last_left_click = None;
                        return;
                    }
                }
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
                    // 同じロックで viewport 先頭 / alt 状態も取得して abs 変換する。
                    let info = app.panes.get(&pid).and_then(|pane| {
                        pane.parser.lock().ok().map(|parser| {
                            let s = parser.screen();
                            (
                                line_text_range(s, ly),
                                crate::pane::viewport_top_abs(s),
                                s.alternate_screen(),
                                s.reset_generation(),
                            )
                        })
                    });
                    if let Some((Some((start_x, end_x)), top, alt, grid_gen)) = info {
                        app.selection = Some(crate::app::Selection {
                            pane_id: pid,
                            anchor: (start_x, top + ly as i64),
                            cursor: (end_x, top + ly as i64),
                            dragging: false,
                            auto_scroll: 0,
                            alt,
                            grid_gen,
                        });
                    } else {
                        // 空行など実テキストが無いときは選択しない。
                        app.selection = None;
                    }
                    // 連続トリプル化を防ぐためリセット。
                    app.last_left_click = None;
                } else if let Some((top, alt, grid_gen)) = pane_view_state(app, pid) {
                    app.selection = Some(crate::app::Selection {
                        pane_id: pid,
                        anchor: (lx, top + ly as i64),
                        cursor: (lx, top + ly as i64),
                        dragging: true,
                        auto_scroll: 0,
                        alt,
                        grid_gen,
                    });
                    app.last_left_click = Some((now, pid, ly));
                    // 離すと選択は消えるが、次の Alt / Shift + クリックの始点に使う。
                    app.click_anchor = Some(crate::app::ClickAnchor {
                        pane_id: pid,
                        pos: (lx, top + ly as i64),
                        alt,
                        grid_gen,
                        size: (rect.w, rect.h),
                    });
                } else {
                    app.selection = None;
                    app.last_left_click = None;
                }
            } else {
                app.selection = None;
                app.last_left_click = None;
            }
        }
        Drag(MouseButton::Left) => {
            // ダブルクリックで作られた行選択 (dragging=false) は微細なマウス
            // ジッタで壊さない。手動ドラッグ中 (dragging=true) のみ cursor 追従。
            if let Some(sel) = app.selection.filter(|s| s.dragging) {
                if let Some(rect) = pane_rects.get(&sel.pane_id) {
                    let lx = (mx - rect.x).clamp(0, rect.w.saturating_sub(1)) as u16;
                    // ペイン外側 (上端より上 / 下端より下) では cursor を端に
                    // 張り付かせつつ auto_scroll 方向を立てる。tick 駆動で
                    // 1 行/30ms スクロールしながら選択を伸ばす。
                    let (ly, dir) = if my < rect.y {
                        (0i32, -1)
                    } else if my >= rect.y + rect.h {
                        (rect.h.saturating_sub(1).max(0), 1)
                    } else {
                        (my - rect.y, 0)
                    };
                    // 現在の viewport 先頭で画面 row → abs row に変換。
                    if let Some((top, _, _)) = pane_view_state(app, sel.pane_id) {
                        if let Some(s) = app.selection.as_mut() {
                            s.cursor = (lx, top + ly as i64);
                            s.auto_scroll = dir;
                        }
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
    // (3)(4) このペインの内側アプリのマウスモード/エンコーディング/alt 画面状態と
    // viewport 先頭 abs 行・reset 世代 (選択の abs 変換用) を 1 回のロックで取得。
    let Some((mode, enc, alt, top, grid_gen)) = app.panes.get(&pid).and_then(|p| {
        p.parser.lock().ok().map(|g| {
            let s = g.screen();
            (
                s.mouse_protocol_mode(),
                s.mouse_protocol_encoding(),
                s.alternate_screen(),
                crate::pane::viewport_top_abs(s),
                s.reset_generation(),
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
    // (7) Shift / Ctrl は強制ローカル。Alt は左ボタンだけ (選択を伸ばすクリック)。
    if mouse_forced_local(me.kind, me.modifiers) {
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

    // 縦ホイールをここまで通過した = alt 画面の子へ転送される。子は自前で内容を
    // 動かす (フル再描画) ため ccnest からは追跡不能で、ローカル選択は内容から
    // 剥離して画面に取り残される。転送前に破棄する (any-key-clears と同じ哲学)。
    if mouse_event_reserved_for_local_scrollback(me)
        && app.selection.as_ref().is_some_and(|s| s.pane_id == pid)
    {
        app.selection = None;
    }
    // 控えてあるクリック位置 (選択を伸ばすときの始点) も同じ理由で捨てる。
    if mouse_event_reserved_for_local_scrollback(me)
        && app.click_anchor.is_some_and(|a| a.pane_id == pid)
    {
        app.click_anchor = None;
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
            // 子へ渡るクリックでも、次の Alt / Shift + クリックの始点に使う。
            app.click_anchor = Some(crate::app::ClickAnchor {
                pane_id: pid,
                pos: (lx, top + ly as i64),
                alt,
                grid_gen,
                size: (rect.w, rect.h),
            });
            true
        }
        Drag(MouseButton::Left) => {
            if let Some(p) = app.pending_mouse {
                if p.pid == pid && (p.lx != lx || p.ly != ly) {
                    // 別セルへ移動 = ドラッグ選択確定。ローカルへ委譲する。
                    // 既存 Drag(Left) アームは selection 既存前提なのでここで生成。
                    // top は現在値を使う (Down〜Drag の間にストリーミング出力が
                    // 行を押し流した場合 anchor がその分ズレるのは許容。alt 画面
                    // では top 恒常 0 で正確)。
                    app.pending_mouse = None;
                    app.mouse_local_drag = true;
                    app.current_tab_mut().focused = pid;
                    app.selection = Some(crate::app::Selection {
                        pane_id: pid,
                        anchor: (p.lx, top + p.ly as i64),
                        cursor: (lx, top + ly as i64),
                        dragging: true,
                        auto_scroll: 0,
                        alt,
                        grid_gen,
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
    // ボタン無しの移動は「セルが変わったときだけ」転送する。Windows のコンソールは
    // ピクセル単位の移動ごとにレコードを出すので、AnyMotion (?1003h) を有効にした
    // 子 (Claude Code のダイアログ中など) にそのまま流すと同じセルの報告が
    // 毎秒数百件になり、子の入力処理を詰まらせる。
    if me.kind == crossterm::event::MouseEventKind::Moved {
        if app.last_forwarded_move == Some((pid, lx, ly)) {
            return true;
        }
        app.last_forwarded_move = Some((pid, lx, ly));
    } else {
        app.last_forwarded_move = None;
    }
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

/// タブバーの描画矩形列 (`draw_tabbar` が毎フレーム記録) から (mx,my) が乗っている
/// タブの index を返す純粋関数。返すのは描画時に格納した index であって Vec 内の
/// 位置ではない (右端に収まらないタブは記録されないため)。右端・下端は排他。
fn tab_at(tab_rects: &[(Rect, usize)], mx: i32, my: i32) -> Option<usize> {
    tab_rects
        .iter()
        .find(|(r, _)| mx >= r.x && mx < r.x + r.w && my >= r.y && my < r.y + r.h)
        .map(|(_, idx)| *idx)
}

/// 純粋関数: `tab_rects` のうち、タブ `dragged` と同じ組 (`group_of(i)` が同じ) の
/// 矩形だけを返す。並べ替えの行き先を同じ組の中に限るために使う。
fn same_group_rects<G: PartialEq>(
    tab_rects: &[(Rect, usize)],
    dragged: usize,
    group_of: impl Fn(usize) -> G,
) -> Vec<(Rect, usize)> {
    let group = group_of(dragged);
    tab_rects
        .iter()
        .copied()
        .filter(|(_, i)| group_of(*i) == group)
        .collect()
}

/// タブを引いている最中、ポインタの横位置 `mx` から「引いているタブ `dragged` を
/// どの位置へ動かすか」を返す純粋関数。動かさないなら None。縦位置は見ない。
///
/// 動かす条件は 2 つとも満たすこと:
/// - ポインタが動かす先のタブに入っている (自分の上や区切りの上では動かさない)
/// - 動かしたあと、引いているタブがポインタの下に来る
///
/// 2 つ目が無いと、狭いタブを広いタブの向こうへ引いたとき、入れ替えた直後も
/// ポインタが広いタブの上に残り、次の Drag で元へ戻る (往復してちらつく)。
/// 見えているタブの左端より左・右端より右は、先頭・末尾として扱う。
/// 右端に収まらず描かれていないタブ (`tab_rects` に無い) へは動かせない。
fn tab_drop_target(tab_rects: &[(Rect, usize)], dragged: usize, mx: i32) -> Option<usize> {
    let w = tab_rects.iter().find(|(_, i)| *i == dragged)?.0.w;
    // 右へ: 条件を満たすいちばん右のタブ。しきい値はタブの並び順に増えるので、
    // 一気に引いたときは通り越したタブをまとめて飛ばす。
    let right = tab_rects
        .iter()
        .filter(|(r, i)| *i > dragged && mx >= r.x + (r.w - w).max(0))
        .map(|(_, i)| *i)
        .max();
    if right.is_some() {
        return right;
    }
    // 左へ: 条件を満たすいちばん左のタブ。
    tab_rects
        .iter()
        .filter(|(r, i)| *i < dragged && mx < r.x + r.w.min(w))
        .map(|(_, i)| *i)
        .min()
}

/// 右クリックをローカルのコンテキストメニューにするか。現状は常に true
/// (Claude Code 側に右クリック固有の機能が無いため)。子へ転送したいケースが
/// 出てきたらここを絞るだけで切り替えられる。
fn right_click_is_local(_app: &App, _pid: PaneId) -> bool {
    true
}

/// 右クリック位置 (端末絶対セル) にコンテキストメニューを開く。
/// コピーは「右クリックしたペインに選択があるとき」だけ有効。選択そのものには
/// 触れない (メニューを開いても反転は残る)。
fn open_context_menu(app: &mut App, pid: PaneId, mx: i32, my: i32) {
    use crate::app::{ContextMenu, MenuAction, MenuItem};
    let has_sel = app.selection.is_some_and(|s| s.pane_id == pid);
    let items = vec![
        MenuItem {
            label: "コピー",
            action: MenuAction::Copy,
            enabled: has_sel,
        },
        MenuItem {
            label: "貼り付け",
            action: MenuAction::Paste,
            enabled: true,
        },
    ];
    let highlighted = first_enabled(&items).unwrap_or(0);
    app.context_menu = Some(ContextMenu {
        pane_id: pid,
        anchor: (mx.max(0) as u16, my.max(0) as u16),
        items,
        highlighted,
    });
}

/// メニュー項目を実行して閉じる。対象は「右クリックされたペイン」であって
/// フォーカス中ペインではない点に注意。
fn execute_menu_action(app: &mut App, action: crate::app::MenuAction, pid: PaneId) {
    match action {
        crate::app::MenuAction::Copy => {
            // Ctrl+C コピーと同一フロー。グリッド識別ガードは extract 側。
            if let Some(sel) = app.selection.filter(|s| s.pane_id == pid) {
                if let Some(pane) = app.panes.get(&pid) {
                    let text = extract_selected_text(pane, sel);
                    if !text.is_empty() {
                        copy_to_clipboard(&text);
                    }
                }
                app.selection = None;
                app.last_ctrl_c = None;
            }
        }
        crate::app::MenuAction::Paste => {
            // Ctrl+V と同じ判定 (画像なら Claude に ESC v、文字列なら bracketed-paste)。
            paste_clipboard_to_pane(app, pid);
            // 入力と同じ扱いで選択は解除する。
            app.selection = None;
        }
    }
    app.context_menu = None;
}

/// メニューの実描画矩形 (枠込み) と項目数から (枠内か, どの項目上か) を返す
/// 純粋関数。項目 i は内側の行 `y = rect.y + 1 + i`、x は左右枠の内側。
/// rect 未確定 (None: 描画前の初回フレーム) は (false, None)。
fn menu_hit(rect: Option<Rect>, n_items: usize, mx: i32, my: i32) -> (bool, Option<usize>) {
    let Some(r) = rect else {
        return (false, None);
    };
    let inside = mx >= r.x && mx < r.x + r.w && my >= r.y && my < r.y + r.h;
    if !inside {
        return (false, None);
    }
    let over = (mx > r.x && mx < r.x + r.w - 1 && my > r.y && my < r.y + r.h - 1)
        .then(|| (my - r.y - 1) as usize)
        .filter(|i| *i < n_items);
    (true, over)
}

/// コンテキストメニュー開時のマウスイベント分類。純粋関数。
#[derive(Debug, PartialEq, Eq)]
enum MenuMouseAction {
    /// 項目 i をハイライト (hover)。
    Highlight(usize),
    /// 項目 i を実行して閉じる。
    Execute(usize),
    /// 何もしないで飲む (枠上クリック / 無効項目 / 対の Up など)。
    Consume,
    /// メニューを閉じる (外側クリック / ホイール)。イベント自体は飲む。
    Close,
    /// 右クリック: 閉じて新しい位置で開き直す。
    Reopen,
}

fn classify_menu_mouse(
    kind: &crossterm::event::MouseEventKind,
    inside: bool,
    over_item: Option<usize>,
    items: &[crate::app::MenuItem],
) -> MenuMouseAction {
    use crossterm::event::{MouseButton, MouseEventKind::*};
    match kind {
        Down(MouseButton::Right) => MenuMouseAction::Reopen,
        ScrollUp | ScrollDown | ScrollLeft | ScrollRight => MenuMouseAction::Close,
        Down(MouseButton::Left) => match over_item {
            Some(i) if items.get(i).is_some_and(|it| it.enabled) => MenuMouseAction::Execute(i),
            Some(_) => MenuMouseAction::Consume,
            None if inside => MenuMouseAction::Consume,
            None => MenuMouseAction::Close,
        },
        // 中ボタンは実行にも解除にも使わない (対の Up(Middle) は誰も
        // mouse_local_drag を掃除できないため、フラグを立てる操作にしない)。
        Down(_) => MenuMouseAction::Consume,
        Moved | Drag(_) => match over_item {
            Some(i) if items.get(i).is_some_and(|it| it.enabled) => MenuMouseAction::Highlight(i),
            _ => MenuMouseAction::Consume,
        },
        Up(_) => MenuMouseAction::Consume,
    }
}

/// タブバー上のマウス入力の分類。純粋関数。
#[derive(Debug, PartialEq, Eq)]
enum TabMouseAction {
    /// タブ i をアクティブにする (左クリック)。
    Switch(usize),
    /// タブ i を配下ペインごと閉じる (中クリック)。
    Close(usize),
    /// タブ i の名前の入力を始める (左ダブルクリック)。
    Rename(usize),
    /// タブバー処理としては何もしない (タブ外 / リネーム中 / Up・Drag・右・ホイール)。
    Ignore,
}

/// `hit` はヒットしたタブ index (`tab_at`)。リネーム中はコミット先がアクティブ
/// タブなので、切替も閉じるも無視する (別タブにリネームが乗るのを防ぐ)。
/// 中ボタンは Down で確定し、対の Up(Middle) は追跡しない (`classify_menu_mouse`
/// と同じ方針。mouse_local_drag を中ボタンから立てない)。
/// `double` = この左 Down が同じタブへの 2 回目 ([`is_tab_double_click`])。
fn classify_tab_mouse(
    kind: &crossterm::event::MouseEventKind,
    hit: Option<usize>,
    renaming: bool,
    double: bool,
) -> TabMouseAction {
    use crossterm::event::{MouseButton, MouseEventKind::*};
    let Some(i) = hit else {
        return TabMouseAction::Ignore;
    };
    if renaming {
        return TabMouseAction::Ignore;
    }
    match kind {
        Down(MouseButton::Left) if double => TabMouseAction::Rename(i),
        Down(MouseButton::Left) => TabMouseAction::Switch(i),
        Down(MouseButton::Middle) => TabMouseAction::Close(i),
        _ => TabMouseAction::Ignore,
    }
}

/// ダブルクリックとみなす 2 回の押下の間隔 (ペインの行ダブルクリックと同じ)。
const DOUBLE_CLICK: Duration = Duration::from_millis(500);

/// 純粋判定: タブ `hit` への左 Down が、直前のタブクリック `last` (時刻, タブ番号)
/// と合わせてダブルクリックになるか。同じタブを [`DOUBLE_CLICK`] 以内に押した
/// ときだけ真。`last` はタブの並びが変わると捨てられる (`App::last_tab_click`) ので、
/// 並べ替えや閉じたあとの「同じ番号の別のタブ」には当たらない。
fn is_tab_double_click(last: Option<(Instant, usize)>, now: Instant, hit: Option<usize>) -> bool {
    matches!(
        (last, hit),
        (Some((t, i)), Some(h)) if i == h && now.saturating_duration_since(t) <= DOUBLE_CLICK
    )
}

/// 名前の入力 1 手の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
enum RenameOutcome {
    /// まだ入力中。
    Editing,
    /// 確定: `Some(名前)` = その名前を付ける、`None` = 手で付けた名前を外す
    /// (自動の名前に戻す)。
    Commit(Option<String>),
    /// 何も変えずにやめる (Esc / 何も打たずに Enter)。
    Cancel,
}

/// 純粋関数: 入力を確定したときの結果 (Enter と、入力中のタブの外のクリックで共用)。
/// 何も打っていない (`fresh`) なら何も変えない: 今出ている自動の名前を、手で
/// 付けた名前として固めないため。空にして確定 = 手で付けた名前を外す。
fn rename_finish(buf: &str, fresh: bool) -> RenameOutcome {
    if fresh {
        return RenameOutcome::Cancel;
    }
    let name = buf.trim();
    if name.is_empty() {
        RenameOutcome::Commit(None)
    } else {
        RenameOutcome::Commit(Some(name.to_string()))
    }
}

/// 純粋関数: 名前の入力中のキー 1 つ。Explorer の名前変更と同じで、入力を
/// 始めた直後 (`fresh`) は今の名前が丸ごと選ばれている扱い: 最初の 1 文字は
/// 丸ごと置き換え、Backspace は全部消す。
fn rename_key(buf: &mut String, fresh: &mut bool, key: &KeyEvent) -> RenameOutcome {
    match key.code {
        KeyCode::Enter => return rename_finish(buf, *fresh),
        KeyCode::Esc => return RenameOutcome::Cancel,
        KeyCode::Backspace => {
            if *fresh {
                buf.clear();
                *fresh = false;
            } else {
                buf.pop();
            }
        }
        KeyCode::Char(c)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            if *fresh {
                buf.clear();
                *fresh = false;
            }
            buf.push(c);
        }
        _ => {}
    }
    RenameOutcome::Editing
}

/// 純粋関数: 名前の入力中の貼り付け。最初の空でない行だけを入れ、制御文字は
/// 落とす (タブ名は 1 行)。入れる文字が無ければ何もしない (`fresh` もそのまま)。
fn rename_paste(buf: &mut String, fresh: &mut bool, text: &str) {
    let line: String = text
        .lines()
        .map(|l| l.chars().filter(|c| !c.is_control()).collect::<String>())
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default();
    if line.is_empty() {
        return;
    }
    if *fresh {
        buf.clear();
        *fresh = false;
    }
    buf.push_str(&line);
}

/// 名前の入力の結果を反映する (確定なら前面のタブに付けて保存)。
fn apply_rename_outcome(app: &mut App, outcome: RenameOutcome) {
    match outcome {
        RenameOutcome::Editing => return,
        RenameOutcome::Commit(name) => {
            if input_trace_enabled() {
                trace_append_line(&format!("rename_tab idx={} name={name:?}", app.active_tab));
            }
            app.set_tab_user_title(app.active_tab, name);
        }
        RenameOutcome::Cancel => {}
    }
    app.end_rename_tab();
}

/// 名前の入力中の貼り付け (端末側の貼り付け・Ctrl+V) を入力欄へ入れる。
fn paste_into_rename(app: &mut App, text: &str) {
    let mut fresh = app.rename_fresh;
    if let Some(buf) = app.renaming_tab.as_mut() {
        rename_paste(buf, &mut fresh, text);
    }
    app.rename_fresh = fresh;
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

    // クリック行を含む連鎖 [chain_start, chain_end] を求める。端末のソフトラップ
    // だけでなく、アプリ (Claude Code) が長い URL を右端で自分で割った行もつなぐ
    // (コピーと同じ判定 `row_join`)。
    let mut chain_start = row;
    while chain_start > 0 && row_join(screen, chain_start - 1).is_some() {
        chain_start -= 1;
    }
    let mut chain_end = row;
    while chain_end + 1 < rows && row_join(screen, chain_end).is_some() {
        chain_end += 1;
    }

    // 連鎖行の文字列を (char, 表示列, 行) として展開。空セルは半角スペース扱い、
    // 全角の 2 セル目は飛ばす。アプリ折り返しの境目では前の行の末尾の余白と
    // 次の行の字下げを落とす。
    let mut chars: Vec<(char, u16, u16)> = Vec::new();
    let mut drop_indent = false;
    for r in chain_start..=chain_end {
        let mut at_head = true;
        for x in 0..cols {
            let Some(cell) = screen.cell(r, x) else {
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            let contents = cell.contents();
            if contents.is_empty() || contents.chars().all(char::is_whitespace) {
                if !(drop_indent && at_head) {
                    chars.push((' ', x, r));
                }
                continue;
            }
            at_head = false;
            for ch in contents.chars() {
                chars.push((ch, x, r));
            }
        }
        if r == chain_end {
            break;
        }
        match row_join(screen, r) {
            Some(RowJoin::App) => {
                while chars.last().is_some_and(|(c, _, _)| *c == ' ') {
                    chars.pop();
                }
                drop_indent = true;
            }
            Some(RowJoin::SoftDropIndent) => drop_indent = true,
            _ => drop_indent = false,
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
        while end < n && is_url_char(chars[end].0) {
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

/// URL に使える文字 (RFC 3986 の ASCII 文字)。空白・日本語・`<>"` などで URL を終える。
fn is_url_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "-._~:/?#[]@!$&'()*+,;=%".contains(c)
}

/// 行 r と次の行のつながり方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowJoin {
    /// 端末のソフトラップ。そのままつなぐ。
    Soft,
    /// 端末のソフトラップだが、次の行の頭の空白はアプリの字下げ。
    SoftDropIndent,
    /// アプリが右端で自分で割った行。前の行の末尾の余白と次の行の字下げを落とす。
    App,
}

/// 行 r が次の行へ続いているか。判定はコピー (`extract_selected_text_from_parser`)
/// と同じ `app_wrapped` / `app_wrapped_after_autowrap` を使う。
fn row_join(screen: &vt100::Screen, r: u16) -> Option<RowJoin> {
    let (rows, _) = screen.size();
    if r + 1 >= rows {
        return None;
    }
    let shape = |y: u16, soft: bool| RowShape {
        text: String::new(),
        soft_wrapped: soft,
        at_row_end: true,
        traits: app_wrap_traits(screen, y),
    };
    let a = shape(r, screen.row_wrapped(r));
    let b = shape(r + 1, screen.row_wrapped(r + 1));
    if a.soft_wrapped {
        if app_wrapped_after_autowrap(&a, &b) {
            Some(RowJoin::SoftDropIndent)
        } else {
            Some(RowJoin::Soft)
        }
    } else if app_wrapped(&a, &b) {
        Some(RowJoin::App)
    } else {
        None
    }
}

/// E2E 用: `CCNEST_OPEN_URL_LOG` があればブラウザを開かず、URL をそのファイルに追記する。
fn open_url(url: &str) {
    if let Some(path) = std::env::var_os("CCNEST_OPEN_URL_LOG") {
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{url}");
        }
        return;
    }
    open_url_in_browser(url);
}

#[cfg(target_os = "windows")]
fn open_url_in_browser(url: &str) {
    // `cmd /C start "" <url>` は ShellExecute 経由で既定ブラウザを起動する。
    // 第 2 引数の空文字列は start のタイトル引数で、URL に空白がある場合に
    // タイトルとして食われるのを防ぐためのプレースホルダ。
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
}

#[cfg(target_os = "macos")]
fn open_url_in_browser(url: &str) {
    let _ = std::process::Command::new("open").arg(url).spawn();
}

#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
fn open_url_in_browser(url: &str) {
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// 選択範囲内のテキストを vt100 スクリーンから抜き出す。行末のスペースは
/// trim し、行間は '\n' で結合。ただし端末のソフトラップ行と、アプリ
/// (Claude Code 等) が長いパス・URL を右端で割っただけの行 (`app_wrapped`) は
/// 改行を挟まずにつなぐ。
///
/// 選択行はバッファ絶対座標なので、各行を「その行が画面 y=0 に来る scrollback
/// offset」で可視化しながら読み取り、抜き出し終わったら元の offset に復元する。
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
    // グリッド識別ガード: 選択作成時と alt 画面状態 / reset 世代が食い違って
    // いたら、この選択の abs 座標は現在の内容と無関係 → 何もコピーしない。
    // 呼び出し元での事前チェックではなく、抽出と同一の &mut parser (= ロック
    // 保持中で reader スレッドが割り込めない) 下で判定するのが重要 (TOCTOU 防止)。
    {
        let s = parser.screen();
        if s.alternate_screen() != sel.alt || s.reset_generation() != sel.grid_gen {
            return String::new();
        }
    }
    let saved_scrollback = parser.screen().scrollback();
    // parser を &mut で掴んでいる間 reader スレッドは process できないため、
    // total_scrolled_off は抜き出し中不変とみなせる。
    let total = parser.screen().total_scrolled_off() as i64;
    let (start, end) = normalize_range(sel.anchor, sel.cursor);

    // 行ごとの抽出結果と連結判定用の形。soft_wrapped の行は次行と
    // ソフトラップで連結しているので、コピー時に '\n' を挟まず、末尾の
    // trim_end も行わない (URL 末尾文字が削れないように)。
    let mut rows: Vec<RowShape> = Vec::new();
    let mut row = start.1;

    // 各反復で「abs 行 row が画面 y=0 に来るような scrollback offset」をセットし、
    // 連続する可視行 (最大 h 行) を読み取って row を進める。offset は vt100 側で
    // 現在の scrollback 行数にクランプされるため、キャップで追い出された古い行は
    // 保持されている最古の行から読み始める (floor クランプ)。
    while row <= end.1 {
        let want_scrollback = (total - row).max(0);
        parser.set_scrollback(want_scrollback as usize);
        let screen = parser.screen();
        let (h_now, cols_now) = screen.size();
        let h_now = h_now as i64;
        // 現在の scrollback で screen y=0 が指す abs 行。
        let base = total - screen.scrollback() as i64;
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
            // 1) vt100 がこの行を wrapped と mark している (右端まで埋まり次行へ続く)。
            // 2) かつ、行末まで切れずに読み取れている (upper が cols-1)。
            //    範囲末尾の `end` を含む行で upper が短く切られている場合は
            //    URL 文字列としての連続性を保証できないので結合扱いにしない。
            let cols_last = cols_now.saturating_sub(1);
            let at_row_end = upper == cols_last;
            let hard_wrapped = screen.row_wrapped(y_u16);

            // 遅延ワイドラップ検出。
            // vt100 は「行末に残り 1 列しかない所へ全角(幅2)文字が来て次行送りに
            // なった」場合、最終セルを空のまま残し row_wrapped を立てない
            // (tmux 互換。vendor/vt100 screen.rs の has_contents 判定コメント参照)。
            // この場合も論理行としては次行と連続しているので、専用に検出して
            // 結合する。判定は「最終セルが空 / その左は埋まっている / 次行頭が
            // 全角文字 (次行送りされた当の文字)」の 3 点。ハードラップ(明示改行)で
            // たまたま cols-2 まで埋めて改行した行を誤結合しないための絞り込み。
            let last_empty = !screen
                .cell(y_u16, cols_last)
                .is_some_and(|c| c.is_wide_continuation() || c.has_contents());
            let prev_filled = cols_last > 0
                && screen
                    .cell(y_u16, cols_last - 1)
                    .is_some_and(|c| c.is_wide_continuation() || c.has_contents());
            let next_wide = screen.cell((y + 1) as u16, 0).is_some_and(|c| c.is_wide());
            let deferred_wide_wrap = !hard_wrapped
                && at_row_end
                && our_row < end.1
                && y + 1 < h_now
                && last_empty
                && prev_filled
                && next_wide;

            let wrapped_to_next =
                our_row < end.1 && at_row_end && (hard_wrapped || deferred_wide_wrap);
            // 遅延ワイドラップ行は、読み取った末尾のパディング空白 1 個を落として
            // から結合する (次行頭の全角文字と直結させ、余分な空白を残さない)。
            let line = if deferred_wide_wrap {
                line.trim_end_matches(' ').to_string()
            } else {
                line
            };
            rows.push(RowShape {
                text: line,
                soft_wrapped: wrapped_to_next,
                at_row_end,
                traits: app_wrap_traits(screen, y_u16),
            });
            row = our_row + 1;
            progressed = true;
            y += 1;
        }
        if !progressed {
            // scrollback 床に達してもまだ end.1 に届かない異常系。安全のため break。
            break;
        }
    }

    parser.set_scrollback(saved_scrollback);

    // ソフトラップ行は改行・末尾trim無しで結合。アプリ折り返し行は末尾の余白と
    // 次行の字下げを落として改行無しで結合。それ以外は trim_end + '\n'。
    let mut lines: Vec<String> = vec![String::new()];
    let mut strip_indent = false;
    for (i, row) in rows.iter().enumerate() {
        let line = if strip_indent {
            row.text.trim_start()
        } else {
            row.text.as_str()
        };
        strip_indent = false;
        let next = rows.get(i + 1);
        let out = lines.last_mut().expect("lines starts non-empty");
        if row.soft_wrapped {
            // 折り返し行: 末尾の URL/英数字が削れないよう trim せず連結し、改行も挟まない。
            out.push_str(line);
            // 最終列まで埋まったアプリ折り返し (ConPTY が自動折り返しで次行へ進み、
            // 字下げの空白をそのまま書く) なら、次行の字下げは本文ではないので落とす。
            strip_indent = next.is_some_and(|n| app_wrapped_after_autowrap(row, n));
        } else if next.is_some_and(|n| app_wrapped(row, n)) {
            out.push_str(line.trim_end());
            strip_indent = true;
        } else {
            out.push_str(line.trim_end());
            if next.is_some() {
                lines.push(String::new());
            }
        }
    }
    dedent_copied_lines(&mut lines, start.0 > 0);
    lines.join("\n")
}

/// Claude Code の応答の 1 行目に付く記号 (`● ` = 記号 1 桁 + 空白 1 桁)。
const REPLY_MARKER: &str = "● ";

/// コピーした行に共通する字下げ (行頭の空白の最小幅) を落とす。Claude Code は
/// 応答本文を 2 桁の余白付きで描くため、そのまま貼ると全行の頭に空白が付く。
///
/// - 空行は最小幅の計算に入れず、そのまま残す (段落の区切り)。
/// - `first_partial` (選択が行の途中から始まった) なら 1 行目は計算にも削除にも
///   含めない (行頭の字下げを選んでいない)。
/// - 行頭から選んだ 1 行目が `● ` で始まるなら、その 2 桁を字下げとみなす。
///   最小幅が 2 以上なら記号ごと落とし、それ未満なら 1 行目はそのまま残す。
/// - 落とすのは共通の幅だけなので、入れ子の箇条書きの相対的な字下げは残る。
fn dedent_copied_lines(lines: &mut [String], first_partial: bool) {
    let lead_spaces = |s: &str| s.bytes().take_while(|&b| b == b' ').count();
    let marker = !first_partial && lines.first().is_some_and(|l| l.starts_with(REPLY_MARKER));
    let skip_first = |i: usize| i == 0 && first_partial;
    let min = lines
        .iter()
        .enumerate()
        .filter(|(i, l)| !skip_first(*i) && !l.trim().is_empty())
        .map(|(i, l)| if i == 0 && marker { 2 } else { lead_spaces(l) })
        .min()
        .unwrap_or(0);
    if min == 0 {
        return;
    }
    for (i, l) in lines.iter_mut().enumerate() {
        if skip_first(i) || l.trim().is_empty() {
            continue;
        }
        if i == 0 && marker {
            if min >= 2 {
                l.drain(..REPLY_MARKER.len());
            }
            continue;
        }
        // 行頭の空白は 1 バイト文字なので、バイト位置で落としてよい。
        l.drain(..min);
    }
}

/// アプリ折り返しの判定で「右端に届いた」とみなす許容幅 (列数)。
///
/// Claude Code は長い行を端末の自動折り返しに任せず、自分で幅を計算して
/// 改行と字下げを出力する (ConPTY 経由では `CR` + `CSI n C` + `CSI 1 B`)。
/// 実測 (Claude Code v2.1.289 / 89 桁): 応答本文は右端の列まで埋めて割り、
/// 入力欄のエコーは右に 1 桁空けて割る。全角文字が残り 1 桁に入らず次行へ
/// 送られる分をさらに +1 して 2 とする。
const APP_WRAP_EDGE_SLACK: u16 = 2;

/// コピー時の 1 行ぶんの抽出結果と、次行との連結判定に使う形。
struct RowShape {
    text: String,
    /// 端末のソフトラップ (row_wrapped / 遅延ワイドラップ) で次行へ続く。
    soft_wrapped: bool,
    /// 選択範囲が行の右端まで届いている (範囲の終端で切れていない)。
    at_row_end: bool,
    traits: AppWrapTraits,
}

/// 画面の 1 行を見て決まる、アプリ折り返し判定用の特徴。隣の行は見ないので、
/// scrollback を窓ごとに読む抽出ループの境目でも同じ結果になる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AppWrapTraits {
    /// 最後の非空白セルが右端から APP_WRAP_EDGE_SLACK 列以内にある。
    reaches_edge: bool,
    /// 行末の語 (空白を含まない文字の並び) が、割られた長い文字列らしい。
    /// 英数字か CJK を含み、かつ「行の本文の先頭から始まる 1 語」か
    /// 「`\` か `/` を含む (パス・URL)」。
    tail_splittable: bool,
    /// 行頭の空白セル数。
    lead_ws: u16,
    /// 本文が始まる列 (行頭の空白と箇条書き記号を飛ばした位置)。アプリの
    /// 続き行はこの列に字下げして置かれる。
    body_indent: u16,
    /// 前の行の続きとして連結してはいけない行頭 (空行・箇条書き記号・
    /// 新しいパス/URL の始まり)。
    head_blocks_join: bool,
    /// 最後の非空白セルが全角 (CJK 等の幅 2) の文字。
    tail_wide: bool,
    /// 最初の非空白セルが全角 (CJK 等の幅 2) の文字。
    head_wide: bool,
}

/// 行 a の末尾と行 b の先頭が、アプリ (Claude Code 等) が長い語を右端で
/// 割っただけの「見た目の改行」かどうか。
fn app_wrapped(a: &RowShape, b: &RowShape) -> bool {
    let long_word = a.traits.tail_splittable && b.traits.lead_ws >= a.traits.lead_ws;
    !a.soft_wrapped
        && a.at_row_end
        && a.traits.reaches_edge
        && !b.traits.head_blocks_join
        && (long_word || continues_wide_prose(a, b))
}

/// 行 b が、行 a の日本語 (全角混じり) の文の、右端で割られた続きらしい。
///
/// Claude Code は日本語の文を空白の位置に関係なく右端で割り、続きを本文の列
/// (`body_indent`) に字下げして置く。境目のどちらかが全角文字で、b がちょうど
/// a の本文の列から始まるなら、空白も改行も挟まずにつなぐ。英文 (境目が両方
/// 半角) は単語の区切りで割られているので対象外。字下げの無い行 (シェル出力)
/// も対象外にして、たまたま右端に届いた行を誤ってつながないようにする。
fn continues_wide_prose(a: &RowShape, b: &RowShape) -> bool {
    (a.traits.tail_wide || b.traits.head_wide)
        && a.traits.body_indent >= 1
        && b.traits.lead_ws == a.traits.body_indent
}

/// 行 a が端末のソフトラップで行 b へ続いているが、実体はアプリ (Claude Code
/// 等) の折り返しで、b の行頭の空白がアプリの字下げかどうか。
///
/// Claude Code が長い語を **最終列まで** 埋めて割ると、ConPTY は `CR` +
/// カーソル移動ではなく端末の自動折り返しのまま次行へ進み、続き行の字下げを
/// 空白として書く (ユーザー報告 2026-10-10: `…247e  4b44…` と空白 2 つが混ざる)。
/// 字下げなら a の本文位置 (`body_indent`) と同じ幅で、b の本文はその直後から
/// 始まる。字下げの無い行 (`body_indent == 0`、シェル出力の素のソフトラップ) や
/// 幅が違う空白は本文の一部とみなし、落とさない。
fn app_wrapped_after_autowrap(a: &RowShape, b: &RowShape) -> bool {
    a.soft_wrapped
        && !b.traits.head_blocks_join
        && ((a.traits.tail_splittable
            && a.traits.body_indent >= 1
            && b.traits.lead_ws == a.traits.body_indent)
            || continues_wide_prose(a, b))
}

/// 行 y のアプリ折り返し判定用の特徴を画面セルから求める。
fn app_wrap_traits(screen: &vt100::Screen, y: u16) -> AppWrapTraits {
    let (_, cols) = screen.size();
    // 列ごとの文字。空セル / 空白は None、全角の 2 セル目は直前の文字と同じ語の一部。
    let cells: Vec<Option<char>> = (0..cols)
        .map(|x| match screen.cell(y, x) {
            Some(c) if c.is_wide_continuation() => Some('\0'),
            Some(c) => c.contents().chars().next().filter(|ch| !ch.is_whitespace()),
            None => None,
        })
        .collect();
    let blank = AppWrapTraits {
        reaches_edge: false,
        tail_splittable: false,
        lead_ws: cols,
        body_indent: cols,
        head_blocks_join: true,
        tail_wide: false,
        head_wide: false,
    };
    let Some(first) = cells.iter().position(Option::is_some) else {
        return blank;
    };
    let last = cells.iter().rposition(Option::is_some).unwrap_or(first);

    // 行末の語 [tail_start, last]。
    let tail_start = cells[..=last]
        .iter()
        .rposition(Option::is_none)
        .map_or(0, |p| p + 1);
    let tail: Vec<char> = cells[tail_start..=last]
        .iter()
        .flatten()
        .copied()
        .filter(|&c| c != '\0')
        .collect();
    let body_start = skip_list_markers(&cells, first);
    let tail_splittable = tail.iter().any(|c| c.is_alphanumeric())
        && (tail_start == body_start || tail.iter().any(|&c| c == '\\' || c == '/'));

    // 全角文字の 2 セル目は Some('\0')。1 セル目は vt100 の is_wide で判る。
    let is_wide_at =
        |x: usize| cells[x] == Some('\0') || screen.cell(y, x as u16).is_some_and(|c| c.is_wide());

    AppWrapTraits {
        reaches_edge: last as u16 + 1 + APP_WRAP_EDGE_SLACK >= cols,
        tail_splittable,
        lead_ws: first as u16,
        body_indent: body_start as u16,
        head_blocks_join: body_start != first
            || starts_new_path(&cells[first..])
            || starts_list_item_without_space(&cells[first..]),
        tail_wide: is_wide_at(last),
        head_wide: is_wide_at(first),
    }
}

/// 後ろに空白を置かずに本文が続く、日本語の文でよく使う箇条書きの行頭
/// (`・項目` `※注記` `①項目` `1）項目` `１．項目` など)。`skip_list_markers` は
/// 「記号 + 空白」の形だけを見るので、こちらで別に拾う。行頭を前の行の続きと
/// みなさないための目印にだけ使い、本文の列 (`body_indent`) は変えない。
fn starts_list_item_without_space(cells: &[Option<char>]) -> bool {
    let head: Vec<char> = cells
        .iter()
        .take(8)
        .map_while(|c| *c)
        .filter(|&c| c != '\0')
        .collect();
    let Some(&c0) = head.first() else {
        return false;
    };
    let bullet = matches!(
        c0,
        '・' | '•'
            | '●'
            | '○'
            | '◦'
            | '■'
            | '□'
            | '◆'
            | '◇'
            | '▪'
            | '▶'
            | '※'
            | '★'
            | '☆'
    ) || ('①'..='⑳').contains(&c0);
    let digits = head
        .iter()
        .take_while(|c| c.is_ascii_digit() || ('０'..='９').contains(*c))
        .count();
    // 半角の `)` `.` は直後が全角のときだけ (`1.5` や `1.txt` を番号とみなさない)。
    let numbered = (1..=3).contains(&digits)
        && match head.get(digits) {
            Some('）' | '．' | '、') => true,
            Some(')' | '.') => head.get(digits + 1).is_some_and(|c| !c.is_ascii()),
            _ => false,
        };
    bullet || numbered
}

/// 行頭の箇条書き記号 (`●` `-` `*` `⎿` `│` などの 1 文字記号、または `1.` `12)`
/// のような番号) とその後ろの空白を飛ばし、本文が始まる列を返す。記号が
/// 無ければ `first` をそのまま返す。`⎿  - item` のような重ね書きも飛ばす。
fn skip_list_markers(cells: &[Option<char>], first: usize) -> usize {
    let mut pos = first;
    loop {
        // 語 [pos, end) を取り出す。
        let end = cells[pos..]
            .iter()
            .position(Option::is_none)
            .map_or(cells.len(), |p| pos + p);
        if end == cells.len() {
            // 後ろに空白が無い = 記号＋空白の形ではない。
            return pos;
        }
        let word: Vec<char> = cells[pos..end]
            .iter()
            .flatten()
            .copied()
            .filter(|&c| c != '\0')
            .collect();
        let is_symbol = word.len() == 1 && !word[0].is_alphanumeric();
        let is_number = word.len() >= 2
            && matches!(word[word.len() - 1], '.' | ')')
            && word[..word.len() - 1].iter().all(|c| c.is_ascii_digit());
        if !(is_symbol || is_number) {
            return pos;
        }
        match cells[end..].iter().position(Option::is_some) {
            Some(p) => pos = end + p,
            None => return pos,
        }
    }
}

/// 行頭が新しいパス / URL の始まり (`C:\` `C:/` `\\server` `http://` `https://`)。
/// 割られた長い語の続きがこの形で始まることはまず無いので、長いパスを 1 行ずつ
/// 並べた一覧を誤ってつながないための目印に使う。
fn starts_new_path(cells: &[Option<char>]) -> bool {
    let head: String = cells
        .iter()
        .take(8)
        .map_while(|c| *c)
        .filter(|&c| c != '\0')
        .collect();
    let b = head.as_bytes();
    let drive =
        b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/');
    drive || head.starts_with("\\\\") || head.starts_with("http://") || head.starts_with("https://")
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
fn normalize_range(a: (u16, i64), b: (u16, i64)) -> ((u16, i64), (u16, i64)) {
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

/// OS クリップボードの中身から、Ctrl+V / 右クリック「貼り付け」が何をするかの
/// 純粋判定。
#[derive(Debug, Clone, PartialEq, Eq)]
enum PasteKind {
    /// 文字列を bracketed-paste で流す。
    Text(String),
    /// 画像: Claude Code に Alt+V 相当 (`ESC v`) を送り、Claude 自身に
    /// クリップボードの画像を読ませる (Windows の Claude Code は Alt+V だけが
    /// 画像ペースト。ccnest は画像の中身には触らない)。
    ImageKey,
    /// どちらも無い (空・Explorer のファイルコピー = CF_HDROP など)。何もしない。
    Nothing,
}

/// 画像があれば画像を優先する。ブラウザの右クリック「画像をコピー」は HTML や
/// 代替文字列を一緒に載せることがあり、文字列優先だとチップにならないため。
/// Claude Code 自身の Alt+V も画像しか見ないので同じ挙動になる。
fn paste_kind(text: Option<String>, has_image: bool) -> PasteKind {
    if has_image {
        return PasteKind::ImageKey;
    }
    match text {
        Some(t) if !t.is_empty() => PasteKind::Text(t),
        _ => PasteKind::Nothing,
    }
}

/// Alt+V と同じバイト列 (`key_to_bytes` の ESC prefix + 'v')。
const IMAGE_PASTE_KEY: &[u8] = b"\x1bv";

/// OS クリップボードにビットマップ画像 (CF_DIB / CF_BITMAP) があるか。
/// `IsClipboardFormatAvailable` はクリップボードを開かずに呼べる。
/// CF_DIB はスクショ (Win+Shift+S)・ブラウザの「画像をコピー」・ペイント等が
/// 載せる形式。ファイルコピー (CF_HDROP) は対象外。
#[cfg(windows)]
fn clipboard_has_image() -> bool {
    use windows_sys::Win32::System::DataExchange::IsClipboardFormatAvailable;
    use windows_sys::Win32::System::Ole::{CF_BITMAP, CF_DIB};
    // SAFETY: 引数はフォーマット定数のみ。グローバル状態の読み取りで、
    // クリップボードの所有権や内容には触れない。
    unsafe {
        IsClipboardFormatAvailable(CF_DIB as u32) != 0
            || IsClipboardFormatAvailable(CF_BITMAP as u32) != 0
    }
}

#[cfg(not(windows))]
fn clipboard_has_image() -> bool {
    false
}

/// OS クリップボードを読んで `pid` のペインへ貼り付ける (Ctrl+V と右クリック
/// 「貼り付け」の共通経路)。画像なら `ESC v` を送って Claude Code に読ませ、
/// 文字列なら bracketed-paste。
fn paste_clipboard_to_pane(app: &App, pid: PaneId) {
    let text = arboard::Clipboard::new()
        .ok()
        .and_then(|mut cb| cb.get_text().ok());
    match paste_kind(text, clipboard_has_image()) {
        PasteKind::Text(t) => paste_to_pane(app, pid, &t),
        PasteKind::ImageKey => {
            if let Some(pane) = app.panes.get(&pid) {
                pane.scroll_to_bottom();
                pane.write(IMAGE_PASTE_KEY);
            }
        }
        PasteKind::Nothing => {}
    }
}

/// ターミナル側から届いたペーストをフォーカス中ペインへ流す
/// (サイドバーにフォーカスがあるときは無視)。
fn handle_paste(app: &mut App, text: &str) {
    if app.sidebar_focused {
        return;
    }
    let focused_id = app.current_tab().focused;
    paste_to_pane(app, focused_id, text);
}

/// ペースト内容を bracketed-paste マーカー `ESC [200~ ... ESC [201~` で包んで
/// 指定ペインの PTY へ書き込む。Claude CLI 等の bracketed-paste 対応
/// 子プロセスは、この区切りを見て「貼り付け」と認識し、途中に含まれる改行を
/// 送信トリガとして扱わずに `[Pasted text +N lines]` のプレースホルダへまとめる。
/// コンテキストメニューの「貼り付け」は右クリックされたペイン (フォーカスとは
/// 限らない) を対象にするため、pid を明示的に受け取る。
fn paste_to_pane(app: &App, pid: PaneId, text: &str) {
    let Some(pane) = app.panes.get(&pid) else {
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

/// マウスモードの子がいても ccnest 側で処理する修飾キーか。Shift / Ctrl は
/// どのマウス入力でもローカル (テキスト選択 / Ctrl+wheel リサイズ / Ctrl+クリック
/// URL)。Alt は左ボタン (押す・引く・離す) だけローカル = 選択を伸ばすクリック。
/// Windows Terminal は Shift 付きのマウス入力を自分の選択に使い、アプリへ
/// 渡さないので、WT の中で届く合図は Alt になる。
fn mouse_forced_local(kind: crossterm::event::MouseEventKind, mods: KeyModifiers) -> bool {
    use crossterm::event::{MouseButton, MouseEventKind};
    if mods.intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL) {
        return true;
    }
    mods.contains(KeyModifiers::ALT)
        && matches!(
            kind,
            MouseEventKind::Down(MouseButton::Left)
                | MouseEventKind::Drag(MouseButton::Left)
                | MouseEventKind::Up(MouseButton::Left)
        )
}

/// 左クリックが「選択をここまで伸ばす」合図か (Alt か Shift。Ctrl 付きは
/// URL を開く側・AltGr = Ctrl+Alt も対象外)。
fn is_extend_click(mods: KeyModifiers) -> bool {
    mods.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT)
        && !mods.contains(KeyModifiers::CONTROL)
}

/// Alt / Shift + 左クリックで作る選択。伸ばせないときは None (呼び出し側は
/// 普通のクリックとして扱う)。`App` 非依存の純粋関数。
///
/// - 同じペイン・同じグリッドの選択が残っている: クリックが選択の中なら始点
///   (anchor) はそのままで終点だけ動かす。選択の外なら、遠い側の端を始点に
///   して選択を丸ごと含むように伸ばす (下へ引いた選択の先をクリック = 始点は
///   そのまま。ダブルクリックの行選択から上へ伸ばしても、その行が欠けない)。
/// - 選択が無い: 直前の普通のクリック位置 (`anchor`) からクリック位置まで。
/// - どちらも使えない (別ペイン / alt 画面の出入り / RIS / ペインの大きさが
///   変わった): None。
fn extended_selection(
    existing: Option<crate::app::Selection>,
    anchor: Option<crate::app::ClickAnchor>,
    pid: PaneId,
    click: (u16, i64),
    alt: bool,
    grid_gen: u64,
    size: (i32, i32),
) -> Option<crate::app::Selection> {
    // (row, col) の辞書順 = 読む順。
    let key = |p: (u16, i64)| (p.1, p.0);
    let start = match existing {
        Some(s) => {
            if s.pane_id != pid || s.alt != alt || s.grid_gen != grid_gen {
                return None;
            }
            let (first, last) = crate::ui::normalize_selection(s.anchor, s.cursor);
            if key(click) < key(first) {
                last
            } else if key(click) > key(last) {
                first
            } else {
                s.anchor
            }
        }
        None => {
            let a = anchor?;
            if a.pane_id != pid || a.alt != alt || a.grid_gen != grid_gen || a.size != size {
                return None;
            }
            a.pos
        }
    };
    Some(crate::app::Selection {
        pane_id: pid,
        anchor: start,
        cursor: click,
        dragging: true,
        auto_scroll: 0,
        alt,
        grid_gen,
    })
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
    // rename 入力中またはコンテキストメニュー開時 (モーダル UI が矢印を消費する)。
    // Defer の 70ms 遅延や recent-wheel Drop で実キーが失われないよう即 Forward
    // する。モーダルを閉じた直後に保留矢印が子へフラッシュされる漏れも防ぐ。
    modal: bool,
    key: &KeyEvent,
    last_wheel_at: Option<Instant>,
    pair_window: Duration,
    now: Instant,
    adjacent_wheel: bool,
) -> ArrowAction {
    if sidebar_focused || modal {
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
            if shift || ctrl || alt {
                // ESC+CR: Claude/Copilot CLI が改行（送信せずに次行）として解釈する標準シーケンス。
                // Alt も含めるのは、Windows Terminal の `shift+enter → sendInput "\u001b\r"`
                // (Claude Code の /terminal-setup 相当) が ConPTY で Alt+Enter の KEY_EVENT に
                // 合成されて届くため。ここで `\r` に落とすと Shift+Enter が送信になってしまう。
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

    /// Ctrl+V の判定: 画像があれば画像 (Claude に ESC v)、無ければ文字列、
    /// どちらも無ければ何もしない。文字列と画像が両方あるときは画像を優先
    /// (ブラウザの「画像をコピー」が代替文字列を載せても画像チップになる)。
    #[test]
    fn paste_kind_prefers_image_then_text_then_nothing() {
        assert_eq!(
            paste_kind(Some("hello".to_string()), false),
            PasteKind::Text("hello".to_string())
        );
        assert_eq!(paste_kind(None, true), PasteKind::ImageKey);
        assert_eq!(
            paste_kind(Some("alt text".to_string()), true),
            PasteKind::ImageKey
        );
        assert_eq!(paste_kind(None, false), PasteKind::Nothing);
        assert_eq!(paste_kind(Some(String::new()), false), PasteKind::Nothing);
    }

    /// 画像のときに送るバイト列は Alt+V を key_to_bytes に通したものと同じ。
    #[test]
    fn image_paste_key_matches_alt_v() {
        let alt_v = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::ALT);
        assert_eq!(key_to_bytes(&alt_v, false), IMAGE_PASTE_KEY.to_vec());
    }

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
    fn parse_ms_override_defaults_on_missing_or_bad_values() {
        assert_eq!(parse_ms_override(None, 8), 8);
        assert_eq!(parse_ms_override(Some(""), 8), 8);
        assert_eq!(parse_ms_override(Some("  "), 8), 8);
        assert_eq!(parse_ms_override(Some("abc"), 8), 8);
        assert_eq!(parse_ms_override(Some("-1"), 8), 8);
        assert_eq!(parse_ms_override(Some("99999"), 8), 8, "上限超えは既定値");
    }

    #[test]
    fn parse_ms_override_accepts_valid_values() {
        assert_eq!(parse_ms_override(Some("0"), 8), 0);
        assert_eq!(parse_ms_override(Some(" 16 "), 8), 16);
        assert_eq!(parse_ms_override(Some("10000"), 8), 10_000);
    }

    fn moved(col: u16, row: u16) -> Event {
        Event::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Moved,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }

    #[test]
    fn only_mouse_moves_detects_pure_motion_batches() {
        assert!(batch_is_only_mouse_moves(&[moved(1, 1)]));
        assert!(batch_is_only_mouse_moves(&[moved(1, 1), moved(2, 1)]));
        assert!(!batch_is_only_mouse_moves(&[]));
        assert!(!batch_is_only_mouse_moves(&[
            moved(1, 1),
            press(KeyCode::Char('a'))
        ]));
        assert!(!batch_is_only_mouse_moves(&[Event::Mouse(
            crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: 1,
                row: 1,
                modifiers: KeyModifiers::NONE,
            }
        )]));
    }

    #[test]
    fn burst_not_extended_for_single_char_press() {
        assert!(!should_extend_burst(&[press(KeyCode::Char('a'))]));
    }

    #[test]
    fn burst_not_extended_for_press_then_release_of_one_key() {
        assert!(!should_extend_burst(&[
            press(KeyCode::Char('a')),
            release(KeyCode::Char('a')),
        ]));
        assert!(!should_extend_burst(&[release(KeyCode::Char('a'))]));
    }

    #[test]
    fn burst_not_extended_for_single_enter_or_tab() {
        assert!(!should_extend_burst(&[press(KeyCode::Enter)]));
        assert!(!should_extend_burst(&[press(KeyCode::Tab)]));
    }

    #[test]
    fn burst_extended_when_two_or_more_presses_queued_together() {
        // ペースト / IME 確定はキューにまとめて積まれるので 1 drain に複数 Press。
        assert!(should_extend_burst(&[
            press(KeyCode::Char('a')),
            press(KeyCode::Char('b')),
        ]));
        assert!(should_extend_burst(&[
            press(KeyCode::Char('a')),
            release(KeyCode::Char('a')),
            press(KeyCode::Char('b')),
        ]));
        assert!(should_extend_burst(&[
            press(KeyCode::Char('x')),
            press(KeyCode::Enter),
        ]));
    }

    #[test]
    fn burst_extended_for_bracketed_paste_event() {
        assert!(should_extend_burst(&[Event::Paste("hello".to_string())]));
    }

    #[test]
    fn burst_not_extended_for_modified_keys() {
        // Ctrl 付きは paste 候補ではない (ショートカット連打を束ねない)。
        assert!(!should_extend_burst(&[
            ctrl_press(KeyCode::Char('d')),
            ctrl_press(KeyCode::Char('d')),
        ]));
        // 矢印など非印字キーも候補外。
        assert!(!should_extend_burst(&[
            press(KeyCode::Up),
            press(KeyCode::Up)
        ]));
    }

    #[test]
    fn format_latency_line_reports_ms_with_total() {
        let line = format_latency_line(12_345, 30_100, 15_600);
        assert_eq!(
            line,
            "latency key_write->output=12.3ms output->draw=30.1ms burst_wait=15.6ms total=58.0ms"
        );
    }

    #[test]
    fn format_latency_line_zero_is_zero() {
        assert_eq!(
            format_latency_line(0, 0, 0),
            "latency key_write->output=0.0ms output->draw=0.0ms burst_wait=0.0ms total=0.0ms"
        );
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

    /// rows = 5 の vt100 にスクロールバックを発生させた上で、可視範囲より上
    /// (スクロールバック内) から始まる abs 座標の選択範囲が scrollback 内の
    /// 行も正しくコピーできることを確認する。下端ドラッグ auto-scroll が
    /// 完了した直後の状態に相当。
    #[test]
    fn extract_selected_text_walks_into_scrollback() {
        let mut parser = vt100::Parser::new(5, 20, 100);
        // line01..line11 を改行付きで、line12 を最後に改行なしで流す。
        // こうすると line12 が live grid の row 4 に座る (末尾空行なし)。
        // 結果: 可視 5 行 = [line08, line09, line10, line11, line12],
        //       scrollback = [line01..line07] (7 行, total_scrolled_off = 7)。
        let mut payload = String::new();
        for i in 1..=11 {
            payload.push_str(&format!("line{:02}\r\n", i));
        }
        payload.push_str("line12");
        parser.process(payload.as_bytes());
        assert_eq!(parser.screen().total_scrolled_off(), 7);

        // lineNN の abs 行 = NN-1 (line01 が最初に流れた行 = abs 0)。
        // anchor = line05 (abs 4, 可視 top より 3 行上), cursor = line12 (abs 11)。
        let sel = crate::app::Selection {
            pane_id: 1,
            anchor: (0, 4),
            cursor: (19, 11),
            dragging: false,
            auto_scroll: 0,
            alt: false,
            grid_gen: 0,
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
            alt: false,
            grid_gen: 0,
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
            alt: false,
            grid_gen: 0,
        };
        let text = extract_selected_text_from_parser(&mut parser, sel);
        // 改行が混入していないこと。URL がそのまま再構成されていること。
        assert!(
            !text.contains('\n'),
            "expected no newline in wrapped URL copy, got: {text:?}"
        );
        assert_eq!(text, url);
    }

    /// 全角(幅2)文字が行末の残り 1 列に入りきらず次行送りになる "遅延ワイド
    /// ラップ"。vt100 はこのケースで row_wrapped を立てない (tmux 互換) ため、
    /// 従来は改行と末尾パディング空白が混入していた。抽出側の専用検出で、
    /// 論理行として改行・余分な空白なしに結合されることを検証する。
    #[test]
    fn extract_selected_text_joins_deferred_wide_wrap() {
        // 40 桁。全角を含むパスが右端で遅延ラップするよう構成。
        // y=0 は "...教科" の直後に全角 "書" が入りきらず次行送り → 最終セル空 /
        // row_wrapped=false になる (この前提が崩れたら本テストの意義も要再検討)。
        let mut parser = vt100::Parser::new(4, 40, 100);
        let path = "C:\\work\\img\\2026-07-18_整体院に学ぶ教科書級マーケティング_thumb.png";
        parser.process(path.as_bytes());
        // 前提の確認: y=0 は vt100 の仕様で wrapped フラグが立たない。
        assert!(
            !parser.screen().row_wrapped(0),
            "前提が変化: y=0 が wrapped 扱いになった"
        );
        let sel = crate::app::Selection {
            pane_id: 1,
            anchor: (0, 0),
            cursor: (39, 1),
            dragging: false,
            auto_scroll: 0,
            alt: false,
            grid_gen: 0,
        };
        let text = extract_selected_text_from_parser(&mut parser, sel);
        assert!(!text.contains('\n'), "改行が混入: {text:?}");
        assert!(!text.contains("  "), "余分な空白が混入: {text:?}");
        assert_eq!(text, path);
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
            alt: false,
            grid_gen: 0,
        };
        let text = extract_selected_text_from_parser(&mut parser, sel);
        assert_eq!(text, "first\nsecond\nthird");
    }

    /// スクロールバック容量のキャップで追い出された行を含む選択は、保持されて
    /// いる最古の行から floor クランプで抽出される (追い出し済みの行は静かに
    /// スキップし、パニックも空返しもしない)。
    #[test]
    fn extract_selected_text_clamps_to_evicted_floor() {
        let mut parser = vt100::Parser::new(3, 20, 4); // 容量 4
        let mut payload = String::new();
        for i in 1..=9 {
            payload.push_str(&format!("line{:02}\r\n", i));
        }
        payload.push_str("line10");
        parser.process(payload.as_bytes());
        // total=7。保持: scrollback = line04..line07, 可視 = line08..line10。
        // lineNN の abs 行 = NN-1。line01 (abs 0) は追い出し済み。
        let sel = crate::app::Selection {
            pane_id: 1,
            anchor: (0, 0),  // line01 (evicted)
            cursor: (19, 9), // line10
            dragging: false,
            auto_scroll: 0,
            alt: false,
            grid_gen: 0,
        };
        let text = extract_selected_text_from_parser(&mut parser, sel);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines,
            vec!["line04", "line05", "line06", "line07", "line08", "line09", "line10"],
            "got: {:#?}",
            lines
        );
        assert_eq!(parser.screen().scrollback(), 0);
    }

    /// live grid の行を指す選択が、保存時 scrollback offset > 0 (ユーザが履歴を
    /// 見ている状態) でも正しく読めて、offset が元どおり復元される。
    #[test]
    fn extract_selected_text_reads_live_rows_with_saved_offset() {
        let mut parser = vt100::Parser::new(3, 20, 100);
        let mut payload = String::new();
        for i in 1..=5 {
            payload.push_str(&format!("line{:02}\r\n", i));
        }
        payload.push_str("line06");
        parser.process(payload.as_bytes());
        // total=3。可視 = [line04, line05, line06] (abs 3..5)。
        parser.set_scrollback(2);
        let sel = crate::app::Selection {
            pane_id: 1,
            anchor: (0, 4),  // line05 (live grid)
            cursor: (19, 5), // line06 (live grid)
            dragging: false,
            auto_scroll: 0,
            alt: false,
            grid_gen: 0,
        };
        let text = extract_selected_text_from_parser(&mut parser, sel);
        assert_eq!(text, "line05\nline06");
        assert_eq!(parser.screen().scrollback(), 2, "offset は元値へ復元される");
    }

    /// グリッド識別 (alt 画面状態 / RIS reset 世代) が選択作成時と食い違う場合、
    /// 抽出は何も返さない (チェックは抽出と同一ロック下 = TOCTOU ガードの検証)。
    #[test]
    fn extract_selected_text_refuses_mismatched_grid() {
        let mut parser = vt100::Parser::new(5, 20, 100);
        parser.process(b"alpha\r\nbeta");

        // alt 不一致: primary 画面なのに alt=true で作られた選択。
        let sel_alt = crate::app::Selection {
            pane_id: 1,
            anchor: (0, 0),
            cursor: (19, 1),
            dragging: false,
            auto_scroll: 0,
            alt: true,
            grid_gen: 0,
        };
        assert_eq!(extract_selected_text_from_parser(&mut parser, sel_alt), "");

        // 世代不一致: RIS (ESC c) 後に旧世代の選択でコピーしようとする。
        let sel_old_gen = crate::app::Selection {
            alt: false,
            grid_gen: 0,
            ..sel_alt
        };
        parser.process(b"\x1bc");
        assert_eq!(parser.screen().reset_generation(), 1);
        assert_eq!(
            extract_selected_text_from_parser(&mut parser, sel_old_gen),
            ""
        );
    }

    // -- クリックした所まで選択を伸ばす (Alt / Shift + 左クリック) --

    const PANE_SIZE: (i32, i32) = (80, 24);

    fn click_anchor_at(pos: (u16, i64)) -> crate::app::ClickAnchor {
        crate::app::ClickAnchor {
            pane_id: 1,
            pos,
            alt: false,
            grid_gen: 0,
            size: PANE_SIZE,
        }
    }

    /// (anchor, cursor) を行優先で並べ直した選択範囲。
    fn span(sel: Option<crate::app::Selection>) -> Option<((u16, i64), (u16, i64))> {
        sel.map(|s| crate::ui::normalize_selection(s.anchor, s.cursor))
    }

    #[test]
    fn trace_shows_mouse_modifiers_only_when_present() {
        let click = |modifiers| {
            Event::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: 10,
                row: 5,
                modifiers,
            })
        };
        assert_eq!(
            fmt_event(&click(KeyModifiers::NONE)),
            "Mouse(Down(Left)@10,5)"
        );
        let alt = fmt_event(&click(KeyModifiers::ALT));
        assert!(alt.starts_with("Mouse(Down(Left)@10,5,"), "got: {alt}");
        assert!(alt.contains("ALT"), "got: {alt}");
    }

    #[test]
    fn extend_click_needs_alt_or_shift_without_ctrl() {
        assert!(is_extend_click(KeyModifiers::ALT));
        assert!(is_extend_click(KeyModifiers::SHIFT));
        assert!(is_extend_click(KeyModifiers::ALT | KeyModifiers::SHIFT));
        // 普通のクリックは今までどおり (新しい選択の始まり / 選択の解除)。
        assert!(!is_extend_click(KeyModifiers::NONE));
        // Ctrl+クリックは URL を開く。AltGr (Ctrl+Alt) も伸ばす合図にしない。
        assert!(!is_extend_click(KeyModifiers::CONTROL));
        assert!(!is_extend_click(KeyModifiers::CONTROL | KeyModifiers::ALT));
    }

    #[test]
    fn extend_moves_the_end_of_an_existing_selection() {
        // (5,10) から (20,12) へ引いた選択。その先 (30,40) をクリック。
        let sel = sel_range((5, 10), (20, 12));
        let out = extended_selection(Some(sel), None, 1, (30, 40), false, 0, PANE_SIZE);
        let out = out.expect("選択が伸びる");
        assert_eq!(out.anchor, (5, 10), "始点はそのまま");
        assert_eq!(out.cursor, (30, 40), "終点がクリック位置");
        assert!(out.dragging, "そのまま引けば終点がついてくる");
        assert_eq!(out.pane_id, 1);
    }

    #[test]
    fn extend_inside_an_existing_selection_shrinks_it() {
        let sel = sel_range((5, 10), (30, 40));
        let out = extended_selection(Some(sel), None, 1, (8, 20), false, 0, PANE_SIZE);
        assert_eq!(span(out), Some(((5, 10), (8, 20))));
    }

    #[test]
    fn extend_from_the_last_plain_click_when_nothing_is_selected() {
        // 頭をクリック (選択は残らない) → 終わりを Alt+クリック。
        let anchor = click_anchor_at((3, 100));
        let out = extended_selection(None, Some(anchor), 1, (40, 180), false, 0, PANE_SIZE);
        assert_eq!(span(out), Some(((3, 100), (40, 180))));
        // 上へ向かっても同じ (クリックした 2 点の間)。
        let out = extended_selection(None, Some(anchor), 1, (7, 60), false, 0, PANE_SIZE);
        assert_eq!(span(out), Some(((7, 60), (3, 100))));
        assert_eq!(out.unwrap().anchor, (3, 100));
    }

    #[test]
    fn extend_prefers_the_selection_over_the_older_click() {
        let sel = sel_range((5, 10), (20, 12));
        let anchor = click_anchor_at((0, 0));
        let out = extended_selection(Some(sel), Some(anchor), 1, (30, 40), false, 0, PANE_SIZE);
        assert_eq!(span(out), Some(((5, 10), (30, 40))));
    }

    #[test]
    fn extend_keeps_the_whole_selection_when_clicking_outside_it() {
        // ダブルクリックの行選択 (anchor = 行頭, cursor = 行末) から上へ:
        // 選んだ行が欠けないよう、行末側を始点にする。
        let line = sel_range((4, 50), (60, 50));
        let out = extended_selection(Some(line), None, 1, (10, 30), false, 0, PANE_SIZE);
        assert_eq!(span(out), Some(((10, 30), (60, 50))));
        // 下から上へ引いた選択 (anchor が後ろ) の、さらに下をクリックしても同じ。
        let upward = sel_range((60, 50), (4, 48));
        let out = extended_selection(Some(upward), None, 1, (10, 70), false, 0, PANE_SIZE);
        assert_eq!(span(out), Some(((4, 48), (10, 70))));
    }

    #[test]
    fn extend_refuses_a_start_from_another_pane_or_grid() {
        let anchor = click_anchor_at((3, 100));
        let click = (40, 180);
        // 別のペイン。
        assert!(extended_selection(None, Some(anchor), 2, click, false, 0, PANE_SIZE).is_none());
        // alt 画面に入れ替わった / RIS で作り直された。
        assert!(extended_selection(None, Some(anchor), 1, click, true, 0, PANE_SIZE).is_none());
        assert!(extended_selection(None, Some(anchor), 1, click, false, 1, PANE_SIZE).is_none());
        // ペインの大きさが変わった (行の折り返しが変わり、控えた行がずれる)。
        assert!(extended_selection(None, Some(anchor), 1, click, false, 0, (60, 24)).is_none());
        // 選択も同じ: 別ペインの選択からは伸ばさない。
        let sel = sel_range((5, 10), (20, 12));
        assert!(extended_selection(Some(sel), None, 2, click, false, 0, PANE_SIZE).is_none());
        assert!(extended_selection(Some(sel), None, 1, click, true, 0, PANE_SIZE).is_none());
        // 始点が何も無い。
        assert!(extended_selection(None, None, 1, click, false, 0, PANE_SIZE).is_none());
    }

    #[test]
    fn alt_left_button_stays_local_in_a_mouse_mode_pane() {
        use crossterm::event::MouseButton as B;
        let left = [
            MouseEventKind::Down(B::Left),
            MouseEventKind::Drag(B::Left),
            MouseEventKind::Up(B::Left),
        ];
        for kind in left {
            assert!(mouse_forced_local(kind, KeyModifiers::ALT), "{kind:?}");
            assert!(mouse_forced_local(kind, KeyModifiers::SHIFT), "{kind:?}");
            assert!(mouse_forced_local(kind, KeyModifiers::CONTROL), "{kind:?}");
            assert!(!mouse_forced_local(kind, KeyModifiers::NONE), "{kind:?}");
        }
        // Alt 付きでも、左ボタン以外は今までどおり子へ流す。
        for kind in [
            MouseEventKind::Down(B::Middle),
            MouseEventKind::ScrollUp,
            MouseEventKind::Moved,
        ] {
            assert!(!mouse_forced_local(kind, KeyModifiers::ALT), "{kind:?}");
            assert!(mouse_forced_local(kind, KeyModifiers::SHIFT), "{kind:?}");
        }
    }

    /// アプリ折り返しテスト用の選択 (anchor から cursor まで)。
    fn sel_range(anchor: (u16, i64), cursor: (u16, i64)) -> crate::app::Selection {
        crate::app::Selection {
            pane_id: 1,
            anchor,
            cursor,
            dragging: false,
            auto_scroll: 0,
            alt: false,
            grid_gen: 0,
        }
    }

    /// Claude Code が ConPTY 越しに送ってくる「自前の改行」。実測 (v2.1.289) では
    /// 右端まで埋めた行のあと `CR` + `CSI 2 C` (字下げ分だけ右へ) + `CSI 1 B`
    /// (1 行下へ) で次行に移る。LF も自動折り返しも使わないので row_wrapped は立たない。
    const CLAUDE_NEXT_ROW: &str = "\r\x1b[2C\x1b[1B";

    const LONG_PATH: &str =
        "C:\\work\\received_logs\\a00138_20260930_2348\\follow_list_fetch_ui_e2e\\20261004_112643\\";

    /// ユーザー報告そのもの: 字下げ 2 桁の段落で、長いパスが右端で割られて
    /// 次行も字下げ 2 桁で続く。改行と字下げを入れずに 1 本のパスとしてコピーする。
    #[test]
    fn extract_selected_text_joins_app_wrapped_path() {
        let mut parser = vt100::Parser::new(6, 48, 100);
        let (head, tail) = LONG_PATH.split_at(46);
        parser.process(format!("  {head}\r\n  {tail}").as_bytes());
        assert!(
            !parser.screen().row_wrapped(0),
            "前提: アプリの改行は row_wrapped にならない"
        );
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (47, 1)));
        assert_eq!(text, LONG_PATH);
    }

    /// 応答本文の 1 行目 (`● ` 付き) から割れたパス。ConPTY の実バイト列
    /// (CR + CSI C + CSI B) で次行へ移る形でもつながる。
    #[test]
    fn extract_selected_text_joins_claude_bullet_path_via_cursor_moves() {
        let mut parser = vt100::Parser::new(6, 48, 100);
        let (head, tail) = LONG_PATH.split_at(46);
        parser.process(format!("● {head}{CLAUDE_NEXT_ROW}{tail}").as_bytes());
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (47, 1)));
        assert_eq!(text, LONG_PATH);
    }

    /// 「保存先: C:\...」のようにラベルの後ろから始まり右端で割れたパス
    /// (全角のラベル付き)。
    #[test]
    fn extract_selected_text_joins_app_wrapped_path_after_label() {
        let mut parser = vt100::Parser::new(6, 48, 100);
        // "  保存先: " は 2 + 6 + 2 = 10 桁。残り 38 桁をパスで埋める。
        let (head, tail) = LONG_PATH.split_at(38);
        parser.process(format!("  保存先: {head}\r\n  {tail}").as_bytes());
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (47, 1)));
        assert_eq!(text, format!("保存先: {LONG_PATH}"));
    }

    /// 箇条書きの中で割れたパス。続きの行は箇条書きの本文位置 (4 桁) に揃う。
    #[test]
    fn extract_selected_text_joins_app_wrapped_path_in_list_item() {
        let mut parser = vt100::Parser::new(6, 50, 100);
        // "  - item " は 9 桁。残り 41 桁。
        let (head, tail) = LONG_PATH.split_at(41);
        parser.process(format!("  - item {head}{CLAUDE_NEXT_ROW}  {tail}").as_bytes());
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (49, 1)));
        assert_eq!(text, format!("- item {LONG_PATH}"));
    }

    /// 3 行以上に割れたパスも順につながる。
    #[test]
    fn extract_selected_text_joins_path_split_over_three_rows() {
        let mut parser = vt100::Parser::new(6, 30, 100);
        let (a, rest) = LONG_PATH.split_at(28);
        let (b, c) = rest.split_at(28);
        parser.process(format!("  {a}\r\n  {b}\r\n  {c}").as_bytes());
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (29, 2)));
        assert_eq!(text, LONG_PATH);
    }

    /// 入力エコーは右に 1 桁余白を残して割られる (実測)。右端から 1 桁内側で
    /// 割れた行もつながる。
    #[test]
    fn extract_selected_text_joins_path_wrapped_one_column_short() {
        let mut parser = vt100::Parser::new(6, 48, 100);
        let (head, tail) = LONG_PATH.split_at(45);
        parser.process(format!("  {head}\r\n  {tail}").as_bytes());
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (47, 1)));
        assert_eq!(text, LONG_PATH);
    }

    /// scrollback の読み取り窓の境目 (A が窓の最終行、B が次の窓の先頭) を
    /// またいでも判定が崩れない。
    #[test]
    fn extract_selected_text_joins_app_wrap_across_scrollback_window() {
        let mut parser = vt100::Parser::new(3, 48, 100);
        let (head, tail) = LONG_PATH.split_at(46);
        // abs 0 = x1, abs 1 = x2, abs 2 = パス前半, abs 3 = パス後半, 以降は押し出し用。
        parser.process(format!("x1\r\nx2\r\n  {head}\r\n  {tail}\r\nx3\r\nx4\r\nx5").as_bytes());
        assert!(parser.screen().total_scrolled_off() >= 4);
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (47, 3)));
        assert_eq!(text, format!("x1\nx2\n  {LONG_PATH}"));
    }

    /// 回帰: 箇条書きの次の項目は別の行として改行を残す。
    #[test]
    fn extract_selected_text_keeps_newline_before_next_list_item() {
        let mut parser = vt100::Parser::new(6, 40, 100);
        let first = format!("  - {}", &LONG_PATH[..36]);
        parser.process(format!("{first}\r\n  - next item").as_bytes());
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (39, 1)));
        // 共通の字下げ 2 桁は落ちる (dedent_copied_lines)。
        assert_eq!(text, format!("{}\n- next item", &first[2..]));
    }

    /// 回帰: 長いパスを 1 行ずつ並べた一覧は、次行が新しいパスの始まり
    /// (`C:\`) なのでつながない。
    #[test]
    fn extract_selected_text_keeps_newline_between_listed_paths() {
        let mut parser = vt100::Parser::new(6, 40, 100);
        let p1 = "  C:\\work\\out\\20261004\\shots\\before1.png";
        let p2 = "  C:\\work\\out\\20261004\\shots\\after01.png";
        assert_eq!(p1.len(), 40, "前提: 1 行目は右端まで埋まる");
        parser.process(format!("{p1}\r\n{p2}").as_bytes());
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (39, 1)));
        assert_eq!(text, format!("{}\n{}", &p1[2..], &p2[2..]));
    }

    /// 回帰: 右端に届かない行 (普通の改行) はつながない。
    #[test]
    fn extract_selected_text_keeps_newline_for_short_path_line() {
        let mut parser = vt100::Parser::new(6, 40, 100);
        parser.process(b"  see C:\\work\\a.txt\r\n  b.txt is next");
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (39, 1)));
        assert_eq!(text, "see C:\\work\\a.txt\nb.txt is next");
    }

    /// 回帰: 罫線だけの行 (英数字なし) は右端まで届いてもつながない。
    #[test]
    fn extract_selected_text_keeps_newline_after_rule_line() {
        let mut parser = vt100::Parser::new(6, 40, 100);
        let rule = "─".repeat(40);
        parser.process(format!("{rule}{CLAUDE_NEXT_ROW}text").as_bytes());
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (39, 1)));
        assert_eq!(text, format!("{rule}\n  text"));
    }

    /// 回帰: 字下げが浅くなる次行は別のブロックとみなしてつながない。
    #[test]
    fn extract_selected_text_keeps_newline_when_indent_decreases() {
        let mut parser = vt100::Parser::new(6, 40, 100);
        let (head, _) = LONG_PATH.split_at(36);
        parser.process(format!("    {head}\r\n  next block").as_bytes());
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (39, 1)));
        assert_eq!(text, format!("  {head}\nnext block"));
    }

    /// 回帰: 英文の単語折り返し (行末の単語がパスでも行頭からの 1 語でもない)
    /// はつながない。
    #[test]
    fn extract_selected_text_keeps_newline_for_word_wrapped_prose() {
        let mut parser = vt100::Parser::new(6, 40, 100);
        let line = "  the quick brown fox jumps over a lazy";
        let line = format!("{line}x"); // 40 桁ちょうどにする
        assert_eq!(line.len(), 40);
        parser.process(format!("{line}\r\n  dog and keeps running").as_bytes());
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (39, 1)));
        // 改行は残る。共通の字下げ 2 桁だけ落ちる。
        assert_eq!(text, format!("{}\ndog and keeps running", &line[2..]));
    }

    /// ユーザー報告 (2026-10-10) の形をダミーで再現した、空白を含まない長い
    /// 文字列 (`数値ID:hex|数値ID:hex|…`)。実トークンではない。
    const LONG_TOKEN: &str = "1000000000000000001:0123456789abcdef0123456789abcdef01234567|1000000000000000002:89abcdef0123456789abcdef01234567|1000000000000000003:fedcba9876543210fedcba9876543210fedcba98";

    /// ユーザー報告そのもの: 字下げ 2 桁の段落で、長い文字列が **最終列まで**
    /// 埋まって割れた。この形では ConPTY が CR + カーソル移動ではなく端末の
    /// 自動折り返しで次行へ進み、字下げの空白 2 つをそのまま書く (vt100 の
    /// `row_wrapped` が立つ)。コピーは改行も字下げの空白も入れずに 1 本につなぐ。
    #[test]
    fn extract_selected_text_strips_indent_after_terminal_autowrap() {
        let mut parser = vt100::Parser::new(6, 48, 100);
        let token = &LONG_TOKEN[..90];
        let (head, tail) = token.split_at(46);
        // CR も LF も無し: 46 字で行が埋まった直後に "  " + 続きを書く。
        parser.process(format!("  {head}  {tail}").as_bytes());
        assert!(
            parser.screen().row_wrapped(0),
            "前提: 最終列まで埋めた直後の書き込みは自動折り返しになる"
        );
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (47, 1)));
        assert_eq!(text, token);
    }

    /// 3 行版 (満杯・満杯・途中まで)。報告の 5 本つなぎと同じ形。
    #[test]
    fn extract_selected_text_strips_indent_after_terminal_autowrap_three_rows() {
        let mut parser = vt100::Parser::new(6, 48, 100);
        let token = &LONG_TOKEN[..130];
        let (a, rest) = token.split_at(46);
        let (b, c) = rest.split_at(46);
        parser.process(format!("  {a}  {b}  {c}").as_bytes());
        assert!(parser.screen().row_wrapped(0) && parser.screen().row_wrapped(1));
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (47, 2)));
        assert_eq!(text, token);
    }

    /// `● ` 始まりの応答 1 行目 (本文位置 2 桁) が最終列まで埋まり、次行が
    /// 字下げ 2 桁で続く。
    #[test]
    fn extract_selected_text_strips_indent_after_terminal_autowrap_bullet() {
        let mut parser = vt100::Parser::new(6, 48, 100);
        let token = &LONG_TOKEN[..90];
        let (head, tail) = token.split_at(46);
        parser.process(format!("● {head}  {tail}").as_bytes());
        assert!(parser.screen().row_wrapped(0));
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (47, 1)));
        assert_eq!(text, token);
    }

    /// 箇条書き `  - ` (本文位置 4 桁) の直後から始まる長い文字列が最終列まで
    /// 埋まり、次行が字下げ 4 桁で続く。
    #[test]
    fn extract_selected_text_strips_indent_after_terminal_autowrap_list_item() {
        let mut parser = vt100::Parser::new(6, 50, 100);
        // "  - " は 4 桁。残り 46 桁。
        let token = &LONG_TOKEN[..88];
        let (head, tail) = token.split_at(46);
        parser.process(format!("  - {head}    {tail}").as_bytes());
        assert!(parser.screen().row_wrapped(0));
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (49, 1)));
        assert_eq!(text, format!("- {token}"));
    }

    /// 実測 (Claude Code v2.1.296・ConPTY・ペイン幅 118 桁・2026-10-10) の形。
    /// `● ` の応答 1 行目が最終列まで埋まると、1 つ目の折り返しは `CSI r;3H`
    /// (絶対カーソル移動・`row_wrapped` 立たず) で、2 つ目は最終列まで埋まった
    /// 直後に `  ` + 続きが直接書かれる自動折り返し (`row_wrapped` 立つ)。
    /// 同じ応答の中に両方の形が混ざっても 1 本につながる。
    #[test]
    fn extract_selected_text_joins_claude_reply_mixed_cursor_move_and_autowrap() {
        let mut parser = vt100::Parser::new(6, 48, 100);
        let token = &LONG_TOKEN[..130];
        let (a, rest) = token.split_at(46);
        let (b, c) = rest.split_at(46);
        parser.process(format!("● {a}[2;3H{b}  {c}").as_bytes());
        assert!(
            !parser.screen().row_wrapped(0),
            "前提: 絶対移動では wrapped にならない"
        );
        assert!(
            parser.screen().row_wrapped(1),
            "前提: 直接続けると wrapped になる"
        );
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (47, 2)));
        assert_eq!(text, token);
    }

    /// 回帰: 字下げの無い行 (シェル出力など) の素のソフトラップでは、次行頭の
    /// 空白は本文の一部なので落とさない。
    #[test]
    fn extract_selected_text_keeps_spaces_after_plain_autowrap() {
        let mut parser = vt100::Parser::new(6, 20, 100);
        parser.process(b"abcdefghijklmnopqrst  next");
        assert!(parser.screen().row_wrapped(0));
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (19, 1)));
        assert_eq!(text, "abcdefghijklmnopqrst  next");
    }

    /// 回帰: 自動折り返しの次行が箇条書き記号で始まるなら字下げを落とさない。
    #[test]
    fn extract_selected_text_keeps_spaces_before_list_item_after_autowrap() {
        let mut parser = vt100::Parser::new(6, 20, 100);
        parser.process(b"  abcdefghijklmnopqr  - item");
        assert!(parser.screen().row_wrapped(0));
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (19, 1)));
        assert_eq!(text, "abcdefghijklmnopqr  - item");
    }

    /// 回帰: 次行の字下げが本文位置と違う (2 に対して 4) なら落とさない。
    #[test]
    fn extract_selected_text_keeps_spaces_when_autowrap_indent_differs() {
        let mut parser = vt100::Parser::new(6, 20, 100);
        parser.process(b"  abcdefghijklmnopqr    deeper");
        assert!(parser.screen().row_wrapped(0));
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (19, 1)));
        assert_eq!(text, "abcdefghijklmnopqr    deeper");
    }

    /// 実機で採取した ConPTY の生バイト列 (Claude Code v2.1.296・ペイン幅 118 桁・
    /// conhost 120×30・2026-10-10・`CCNEST_PTY_DUMP=1`)。応答 1 行目 `● ` の後ろに
    /// ダミーのトークン列を出させたもの。1 つ目の折り返しは `CSI 12;3H`、2 つ目は
    /// 最終列まで埋まった直後に `  210|…` が直接続く (自動折り返し)。
    const CAPTURED_REPLY: &str = "\x1b[?25l\x1b[38;2;255;255;255m\x1b[11;1H●\x1b[m\x1b[1C1000000000000000001:0123456789abcdef0123456789abcdef01234567|1000000000000000002:89abcdef0123456789abcdef01234567|10\x1b[12;3H00000000000000003:fedcba9876543210fedcba9876543210fedcba98|1000000000000000004:76543210fedcba9876543210fedcba9876543  210|1000000000000000005:a1b2c3d4e5f60718293a4b5c6d7e8f9001122334";
    const CAPTURED_TOKEN: &str = "1000000000000000001:0123456789abcdef0123456789abcdef01234567|1000000000000000002:89abcdef0123456789abcdef01234567|1000000000000000003:fedcba9876543210fedcba9876543210fedcba98|1000000000000000004:76543210fedcba9876543210fedcba9876543210|1000000000000000005:a1b2c3d4e5f60718293a4b5c6d7e8f9001122334";

    /// 採取した実バイト列をそのまま流し、3 行を選択してコピーすると、改行も
    /// 字下げの空白も入らず元のトークン列と一致する。
    #[test]
    fn extract_selected_text_replays_captured_claude_reply() {
        let mut parser = vt100::Parser::new(30, 118, 100);
        parser.process(CAPTURED_REPLY.as_bytes());
        let screen = parser.screen();
        assert!(!screen.row_wrapped(10), "前提: 1 つ目の折り返しは絶対移動");
        assert!(
            screen.row_wrapped(11),
            "前提: 2 つ目の折り返しは自動折り返し"
        );
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 10), (117, 12)));
        assert_eq!(text, CAPTURED_TOKEN);
        assert!(!text.contains('\n') && !text.contains("  "));
    }

    /// 表示幅 (全角 = 2 桁)。日本語の折り返しテストの行幅の前提確認に使う。
    fn disp_w(s: &str) -> usize {
        unicode_width::UnicodeWidthStr::width(s)
    }

    /// ユーザー報告 (2026-10-27): Claude Code の日本語の応答本文を 3 行に
    /// わたって選択してコピーすると、右端で折り返した位置の改行と字下げ 2 桁が
    /// 入っていた。1 本の文になり、`● ` も字下げも入らない。
    #[test]
    fn extract_selected_text_joins_japanese_paragraph_over_three_rows() {
        let r1 = "お問い合わせありがとうございます。ご指";
        let r2 = "摘いただいた保存ボタンの不具合は、設定";
        let r3 = "画面で直りました。";
        assert_eq!((disp_w(r1), disp_w(r2)), (38, 38), "前提: 本文幅 38 桁");
        let mut parser = vt100::Parser::new(6, 40, 100);
        parser.process(format!("● {r1}{CLAUDE_NEXT_ROW}{r2}{CLAUDE_NEXT_ROW}{r3}").as_bytes());
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (39, 2)));
        assert_eq!(text, format!("{r1}{r2}{r3}"));
    }

    /// 半角の空白を含む日本語の行 (`（@sx09s のポスト 20 件を確認）`) が右端で
    /// 割れても、境目の片側が全角ならつながる。行末の語が「行頭からの 1 語」
    /// ではないので、従来の長い語の判定 (tail_splittable) では拾えなかった形。
    #[test]
    fn extract_selected_text_joins_japanese_line_with_ascii_words() {
        let r1 = "ご依頼の件を確認しました（@sx09s のポ";
        let r2 = "スト 20 件を確認）。その結果、いいねの";
        let r3 = "数が少ない投稿は深夜に偏っていました。";
        // r1 は 37 桁 (残り 1 桁に全角の「ス」が入らず次行へ送られた形)。
        assert_eq!((disp_w(r1), disp_w(r2)), (37, 38));
        let mut parser = vt100::Parser::new(6, 40, 100);
        parser.process(format!("● {r1}{CLAUDE_NEXT_ROW}{r2}{CLAUDE_NEXT_ROW}{r3}").as_bytes());
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (39, 2)));
        assert_eq!(
            text,
            "ご依頼の件を確認しました（@sx09s のポスト 20 件を確認）。その結果、いいねの数が少ない投稿は深夜に偏っていました。"
        );
    }

    /// 最終列まで埋まった日本語の行が端末の自動折り返しで次行へ続き、
    /// ConPTY が字下げの空白 2 つをそのまま書いた形でもつながる。
    #[test]
    fn extract_selected_text_joins_japanese_line_after_terminal_autowrap() {
        let r1 = "スト 20 件を確認）。その結果、いいねの";
        let r2 = "数が少ない投稿は深夜に偏っていました。";
        assert_eq!(disp_w(r1), 38);
        let mut parser = vt100::Parser::new(6, 40, 100);
        parser.process(format!("● {r1}  {r2}").as_bytes());
        assert!(parser.screen().row_wrapped(0), "前提: 自動折り返し");
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (39, 1)));
        assert_eq!(text, format!("{r1}{r2}"));
    }

    /// 段落の間の空行は残り、どちらの段落も字下げが落ちる。
    #[test]
    fn extract_selected_text_keeps_blank_line_between_japanese_paragraphs() {
        let p1a = "設定は 3 か所です。まず画面の右上の歯";
        let p1b = "車を押してください。";
        let p2a = "返信は 1 日 2 回まとめて送ると反応が良";
        let p2b = "くなります。";
        assert_eq!((disp_w(p1a), disp_w(p2a)), (37, 38));
        let mut parser = vt100::Parser::new(8, 40, 100);
        parser.process(
            format!("● {p1a}{CLAUDE_NEXT_ROW}{p1b}\r\n\r\n  {p2a}{CLAUDE_NEXT_ROW}{p2b}")
                .as_bytes(),
        );
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (39, 4)));
        assert_eq!(text, format!("{p1a}{p1b}\n\n{p2a}{p2b}"));
    }

    /// 回帰: 右端まで届いた日本語の行でも、次行が箇条書き (`・` `1）` `- `)
    /// で始まるなら改行を残す。
    #[test]
    fn extract_selected_text_keeps_newline_before_japanese_list_item() {
        let lead = "以下の 3 点を確認しました。どれも問題";
        assert_eq!(disp_w(lead), 37);
        for item in ["・設定画面を開く", "1）設定画面を開く", "- 設定画面を開く"]
        {
            let mut parser = vt100::Parser::new(6, 40, 100);
            parser.process(format!("  {lead}{CLAUDE_NEXT_ROW}{item}").as_bytes());
            let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (39, 1)));
            assert_eq!(text, format!("{lead}\n{item}"), "item: {item}");
        }
    }

    /// 入れ子の箇条書きは、共通の字下げだけ落として相対的な字下げを残す。
    #[test]
    fn extract_selected_text_dedent_keeps_nested_list_indent() {
        let mut parser = vt100::Parser::new(6, 40, 100);
        parser.process("  - 親の項目です\r\n    - 子の項目です\r\n  - 次の親です".as_bytes());
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (39, 2)));
        assert_eq!(text, "- 親の項目です\n  - 子の項目です\n- 次の親です");
    }

    /// 回帰: たまたま右端まで届いた行でも、次行の字下げが本文位置と違えば
    /// つながない (別のブロック)。
    #[test]
    fn extract_selected_text_keeps_newline_when_japanese_indent_differs() {
        let lead = "以下の 3 点を確認しました。どれも問題";
        let mut parser = vt100::Parser::new(6, 40, 100);
        parser.process(format!("  {lead}\r\n      深い字下げの行です").as_bytes());
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (39, 1)));
        assert_eq!(text, format!("{lead}\n    深い字下げの行です"));
    }

    /// 応答 1 行目の `● ` は、行頭から選択したときは字下げの一部として落とす。
    #[test]
    fn extract_selected_text_drops_reply_marker_when_selected_from_col_0() {
        let mut parser = vt100::Parser::new(6, 40, 100);
        parser.process("● 承知しました。すぐに対応します。".as_bytes());
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 0), (39, 0)));
        assert_eq!(text, "承知しました。すぐに対応します。");
    }

    /// 行の途中から選択したときは 1 行目に手を付けず、2 行目以降だけ字下げを落とす。
    #[test]
    fn extract_selected_text_keeps_first_line_when_selection_starts_mid_row() {
        let mut parser = vt100::Parser::new(6, 40, 100);
        parser.process("● 承知しました。\r\n  次の行です。".as_bytes());
        // x=4 は「知」(「承」が 2..=3 桁)。
        let text = extract_selected_text_from_parser(&mut parser, sel_range((4, 0), (39, 1)));
        assert_eq!(text, "知しました。\n次の行です。");
    }

    /// Claude Code の日本語の応答を、ConPTY の出し方 (CAPTURED_REPLY と同じ
    /// 絶対カーソル移動 `CSI r;3H` と、最終列まで埋まった直後の自動折り返し) で
    /// 再現したもの。段落・空行・見出し・字下げ 4 桁で続く箇条書きが混ざっても、
    /// 段落は 1 行に、箇条書きは項目ごとに 1 行になり、共通の字下げは落ちる。
    /// 文面はダミー (実データではない)。
    #[test]
    fn extract_selected_text_replays_emulated_japanese_claude_reply() {
        let a1 = "ご連絡ありがとうございます。v4.12 で「保存」を押しても設定";
        let a2 = "が残らない件、こちらでも再現しました。原因は設定ファイルの";
        let a3 = "書き込み失敗です。次の版で直します。";
        let b0 = "確認した内容：";
        let b1 = "ログイン状態は正常で、予約投稿 12 件もすべて送信済みにな";
        let b1c = "っていました。";
        let b2 = "自動いいねは止まっていません。";
        assert_eq!((disp_w(a1), disp_w(a2), disp_w(b1)), (58, 58, 56));
        let reply = format!(
            "\x1b[?25l\x1b[38;2;255;255;255m\x1b[11;1H●\x1b[m\x1b[1C{a1}\x1b[12;3H{a2}  {a3}\x1b[15;3H{b0}\x1b[16;3H- {b1}\x1b[17;5H{b1c}\x1b[18;3H- {b2}"
        );
        let mut parser = vt100::Parser::new(30, 60, 100);
        parser.process(reply.as_bytes());
        let screen = parser.screen();
        assert!(!screen.row_wrapped(10), "前提: 1 つ目の折り返しは絶対移動");
        assert!(
            screen.row_wrapped(11),
            "前提: 2 つ目の折り返しは自動折り返し"
        );
        let text = extract_selected_text_from_parser(&mut parser, sel_range((0, 10), (59, 17)));
        assert_eq!(
            text,
            format!("{a1}{a2}{a3}\n\n{b0}\n- {b1}{b1c}\n- {b2}"),
            "got: {text:#?}"
        );
    }

    fn mi(enabled: bool) -> crate::app::MenuItem {
        crate::app::MenuItem {
            label: "テスト",
            action: crate::app::MenuAction::Paste,
            enabled,
        }
    }

    #[test]
    fn menu_hit_maps_rows_to_items() {
        // 枠込み 12x4 (項目 2 つ)。項目 0 = y+1 行、項目 1 = y+2 行。
        let r = Rect {
            x: 10,
            y: 6,
            w: 12,
            h: 4,
        };
        assert_eq!(menu_hit(Some(r), 2, 11, 7), (true, Some(0)));
        assert_eq!(menu_hit(Some(r), 2, 11, 8), (true, Some(1)));
        // 枠の上は inside だが項目なし。
        assert_eq!(menu_hit(Some(r), 2, 10, 6), (true, None));
        assert_eq!(menu_hit(Some(r), 2, 21, 9), (true, None));
        // 外側。
        assert_eq!(menu_hit(Some(r), 2, 9, 7), (false, None));
        // 描画前 (rect 未確定)。
        assert_eq!(menu_hit(None, 2, 11, 7), (false, None));
    }

    #[test]
    fn next_enabled_skips_disabled_and_wraps() {
        let items = vec![mi(false), mi(true), mi(true)];
        assert_eq!(next_enabled(&items, 1, 1), 2);
        // index 0 は無効なので wrap して 1 へ。
        assert_eq!(next_enabled(&items, 2, 1), 1);
        assert_eq!(next_enabled(&items, 1, -1), 2);
        let all_off = vec![mi(false), mi(false)];
        assert_eq!(next_enabled(&all_off, 0, 1), 0);
    }

    #[test]
    fn first_enabled_finds_first() {
        assert_eq!(first_enabled(&[mi(false), mi(true)]), Some(1));
        assert_eq!(first_enabled(&[mi(false), mi(false)]), None);
    }

    #[test]
    fn classify_menu_mouse_table() {
        use crossterm::event::{MouseButton, MouseEventKind::*};
        let items = vec![mi(false), mi(true)];
        assert_eq!(
            classify_menu_mouse(&Down(MouseButton::Right), true, Some(1), &items),
            MenuMouseAction::Reopen
        );
        assert_eq!(
            classify_menu_mouse(&ScrollUp, true, Some(1), &items),
            MenuMouseAction::Close
        );
        assert_eq!(
            classify_menu_mouse(&Down(MouseButton::Left), true, Some(1), &items),
            MenuMouseAction::Execute(1)
        );
        assert_eq!(
            classify_menu_mouse(&Down(MouseButton::Left), true, Some(0), &items),
            MenuMouseAction::Consume
        );
        assert_eq!(
            classify_menu_mouse(&Down(MouseButton::Left), true, None, &items),
            MenuMouseAction::Consume
        );
        assert_eq!(
            classify_menu_mouse(&Down(MouseButton::Left), false, None, &items),
            MenuMouseAction::Close
        );
        // 中ボタンは実行にしない (対の Up(Middle) が mouse_local_drag を
        // 掃除できないため)。
        assert_eq!(
            classify_menu_mouse(&Down(MouseButton::Middle), true, Some(1), &items),
            MenuMouseAction::Consume
        );
        assert_eq!(
            classify_menu_mouse(&Moved, true, Some(1), &items),
            MenuMouseAction::Highlight(1)
        );
        assert_eq!(
            classify_menu_mouse(&Moved, true, Some(0), &items),
            MenuMouseAction::Consume
        );
        assert_eq!(
            classify_menu_mouse(&Up(MouseButton::Right), true, None, &items),
            MenuMouseAction::Consume
        );
        assert_eq!(
            classify_menu_mouse(&Up(MouseButton::Left), false, None, &items),
            MenuMouseAction::Consume
        );
    }

    /// tab_at: 左端は含み右端は排他 (区切りの 1 桁は None)、別行は None、空は None。
    #[test]
    fn tab_at_table() {
        let r = |x: i32, w: i32| Rect { x, y: 0, w, h: 1 };
        // サイドバー表示時を模して x=21 起点。タブ 0 は 21..31、区切り 1 桁、
        // タブ 1 は 32..40。
        let rects = vec![(r(21, 10), 0usize), (r(32, 8), 1usize)];
        assert_eq!(tab_at(&rects, 21, 0), Some(0), "左端は含む");
        assert_eq!(tab_at(&rects, 30, 0), Some(0), "最終セル");
        assert_eq!(tab_at(&rects, 31, 0), None, "区切り 1 桁 (右端排他)");
        assert_eq!(tab_at(&rects, 32, 0), Some(1));
        assert_eq!(tab_at(&rects, 39, 0), Some(1));
        assert_eq!(tab_at(&rects, 40, 0), None);
        assert_eq!(tab_at(&rects, 20, 0), None, "サイドバー側");
        assert_eq!(tab_at(&rects, 25, 1), None, "別行");
        assert_eq!(tab_at(&[], 25, 0), None, "空");
    }

    /// tab_at は Vec 内の位置ではなく描画時に格納した index を返す
    /// (右端に収まらないタブは draw_tabbar が記録しないため index が飛ぶ)。
    #[test]
    fn tab_at_returns_recorded_index() {
        let r = |x: i32, w: i32| Rect { x, y: 0, w, h: 1 };
        let rects = vec![(r(0, 10), 0usize), (r(11, 10), 2usize)];
        assert_eq!(tab_at(&rects, 15, 0), Some(2));
    }

    /// 幅 10 のタブ 3 枚 (区切り 1 桁): x = 0..10 / 11..21 / 22..32。
    fn even_tabs() -> Vec<(Rect, usize)> {
        let r = |x: i32, w: i32| Rect { x, y: 0, w, h: 1 };
        vec![(r(0, 10), 0usize), (r(11, 10), 1usize), (r(22, 10), 2usize)]
    }

    /// 並べ替えの行き先は同じ組の中だけ。タブ 1・3 が右へ小さく寄せてあるとき
    /// (タブバーの並び: 0, 2, 4 │ 1, 3)、0 を右へ引いても寄せた組へは入らない。
    #[test]
    fn drag_targets_stay_inside_the_dragged_tabs_group() {
        let r = |x: i32, w: i32| Rect { x, y: 0, w, h: 1 };
        // 左の組 (幅 10): タブ 0, 2, 4。仕切りのあと右の組 (幅 5): タブ 1, 3。
        let rects = vec![
            (r(0, 10), 0usize),
            (r(11, 10), 2),
            (r(22, 10), 4),
            (r(35, 5), 1),
            (r(41, 5), 3),
        ];
        let parked = |i: usize| i == 1 || i == 3;
        let ids = |v: &[(Rect, usize)]| v.iter().map(|(_, i)| *i).collect::<Vec<_>>();
        let left = same_group_rects(&rects, 0, parked);
        assert_eq!(ids(&left), [0, 2, 4]);
        let right = same_group_rects(&rects, 3, parked);
        assert_eq!(ids(&right), [1, 3]);
        // タブ 0 をタブ 2 の上まで引く → 2 の位置へ (間のタブ 1 は別の組なので関係ない)。
        assert_eq!(tab_drop_target(&left, 0, 15), Some(2));
        // 寄せた組の上 (桁 38・43) まで引いても、行き先は左の組の右端 (タブ 4) まで。
        assert_eq!(tab_drop_target(&left, 0, 38), Some(4));
        assert_eq!(tab_drop_target(&left, 0, 43), Some(4));
        // 組を見ないと、寄せたタブが行き先になってしまう。
        assert_eq!(tab_drop_target(&rects, 0, 38), Some(4));
        assert_eq!(tab_drop_target(&rects, 2, 43), Some(4));
        // 寄せた組の中では並べ替えられる (3 を 1 の上へ)。左の組へは出ない。
        assert_eq!(tab_drop_target(&right, 3, 36), Some(1));
        assert_eq!(tab_drop_target(&right, 3, 5), Some(1));
    }

    /// `rects` の並びで `from` を `to` へ動かしたあとの矩形 (index は 0 から振り直し)。
    fn rects_after_move(rects: &[(Rect, usize)], from: usize, to: usize) -> Vec<(Rect, usize)> {
        let mut widths: Vec<i32> = rects.iter().map(|(r, _)| r.w).collect();
        let w = widths.remove(from);
        widths.insert(to, w);
        let mut x = rects[0].0.x;
        widths
            .into_iter()
            .enumerate()
            .map(|(i, w)| {
                let r = Rect { x, y: 0, w, h: 1 };
                x += w + 1;
                (r, i)
            })
            .collect()
    }

    /// 引いているタブの上・区切りの上では動かさない。隣のタブに入ったら動かす。
    #[test]
    fn tab_drop_target_moves_when_pointer_enters_neighbor() {
        let rects = even_tabs();
        assert_eq!(tab_drop_target(&rects, 1, 15), None, "自分の上");
        assert_eq!(tab_drop_target(&rects, 1, 21), None, "右の区切り");
        assert_eq!(tab_drop_target(&rects, 1, 22), Some(2), "右隣に入った");
        assert_eq!(tab_drop_target(&rects, 1, 10), None, "左の区切り");
        assert_eq!(tab_drop_target(&rects, 1, 9), Some(0), "左隣に入った");
    }

    /// 2 つ先まで一気に引いたら、その位置へ。
    #[test]
    fn tab_drop_target_skips_over_tabs() {
        let rects = even_tabs();
        assert_eq!(tab_drop_target(&rects, 0, 25), Some(2));
        assert_eq!(tab_drop_target(&rects, 2, 3), Some(0));
    }

    /// 見えているタブの外まで引いたら、見えている範囲の先頭／末尾へ。
    #[test]
    fn tab_drop_target_clamps_outside_the_bar() {
        let r = |x: i32, w: i32| Rect { x, y: 0, w, h: 1 };
        // サイドバーの右 (x=20) から始まるタブバー。
        let rects = vec![
            (r(20, 10), 0usize),
            (r(31, 10), 1usize),
            (r(42, 10), 2usize),
        ];
        assert_eq!(tab_drop_target(&rects, 1, 5), Some(0), "左端より左");
        assert_eq!(tab_drop_target(&rects, 1, 200), Some(2), "右端より右");
        assert_eq!(
            tab_drop_target(&rects, 0, 5),
            None,
            "先頭はそれ以上左へ行かない"
        );
        assert_eq!(
            tab_drop_target(&rects, 2, 200),
            None,
            "末尾はそれ以上右へ行かない"
        );
    }

    /// 狭いタブを広いタブの向こうへ: 広いタブに入っただけでは動かさない
    /// (動かすとポインタが広いタブの上に残り、次の Drag で戻ってしまう)。
    /// 動かしたあとの矩形で同じ位置を渡しても動かない = 往復しない。
    #[test]
    fn tab_drop_target_narrow_over_wide_does_not_oscillate() {
        let r = |x: i32, w: i32| Rect { x, y: 0, w, h: 1 };
        // 幅 4 (x=0..4) と 幅 20 (x=5..25)。
        let rects = vec![(r(0, 4), 0usize), (r(5, 20), 1usize)];
        assert_eq!(tab_drop_target(&rects, 0, 5), None, "入っただけ");
        assert_eq!(tab_drop_target(&rects, 0, 20), None, "しきい値の手前");
        assert_eq!(tab_drop_target(&rects, 0, 21), Some(1), "5 + 20 - 4 = 21");
        let after = rects_after_move(&rects, 0, 1);
        assert_eq!(after[1].0.x, 21, "動かしたタブはポインタの下に来る");
        assert_eq!(tab_drop_target(&after, 1, 21), None, "同じ位置で戻らない");
        // 逆向き (左へ) も同じ。
        assert_eq!(tab_drop_target(&after, 1, 4), None, "しきい値の手前");
        assert_eq!(tab_drop_target(&after, 1, 3), Some(0), "0 + 4 = 4 より左");
        let back = rects_after_move(&after, 1, 0);
        assert_eq!(tab_drop_target(&back, 0, 3), None, "同じ位置で戻らない");
    }

    /// 広いタブを狭いタブの向こうへ: 狭いタブに入ったら動かす。往復しない。
    /// 広いタブの真ん中を押しただけ (ポインタは自分の上) では左右どちらにも動かない。
    #[test]
    fn tab_drop_target_wide_over_narrow_does_not_oscillate() {
        let r = |x: i32, w: i32| Rect { x, y: 0, w, h: 1 };
        // 幅 3 (x=0..3)・幅 20 (x=4..24)・幅 3 (x=25..28)。
        let rects = vec![(r(0, 3), 0usize), (r(4, 20), 1usize), (r(25, 3), 2usize)];
        for mx in 4..24 {
            assert_eq!(tab_drop_target(&rects, 1, mx), None, "自分の上 mx={mx}");
        }
        assert_eq!(tab_drop_target(&rects, 1, 25), Some(2));
        let after = rects_after_move(&rects, 1, 2);
        assert_eq!(tab_drop_target(&after, 2, 25), None, "同じ位置で戻らない");
        assert_eq!(tab_drop_target(&rects, 1, 2), Some(0));
        let after = rects_after_move(&rects, 1, 0);
        assert_eq!(tab_drop_target(&after, 0, 2), None, "同じ位置で戻らない");
    }

    /// 引いているタブが矩形に無い (右端に収まらず描かれていない) なら動かさない。
    #[test]
    fn tab_drop_target_none_when_dragged_not_drawn() {
        let rects = even_tabs();
        assert_eq!(tab_drop_target(&rects, 5, 3), None);
        assert_eq!(tab_drop_target(&[], 0, 3), None);
    }

    /// タブバー分類: Down(Left)→Switch、2 回目の Down(Left)→Rename、Down(Middle)→Close、
    /// リネーム中はどれも Ignore、タブ外は Ignore、Up/Drag/右/ホイールは Ignore。
    #[test]
    fn classify_tab_mouse_table() {
        use crossterm::event::{MouseButton, MouseEventKind::*};
        assert_eq!(
            classify_tab_mouse(&Down(MouseButton::Left), Some(1), false, false),
            TabMouseAction::Switch(1)
        );
        assert_eq!(
            classify_tab_mouse(&Down(MouseButton::Left), Some(1), false, true),
            TabMouseAction::Rename(1),
            "同じタブへの 2 回目の左 Down は名前の入力"
        );
        assert_eq!(
            classify_tab_mouse(&Down(MouseButton::Middle), Some(1), false, false),
            TabMouseAction::Close(1)
        );
        assert_eq!(
            classify_tab_mouse(&Down(MouseButton::Middle), Some(1), false, true),
            TabMouseAction::Close(1),
            "ダブルクリックは左ボタンだけ"
        );
        assert_eq!(
            classify_tab_mouse(&Down(MouseButton::Left), Some(1), true, false),
            TabMouseAction::Ignore,
            "リネーム中は切替しない"
        );
        assert_eq!(
            classify_tab_mouse(&Down(MouseButton::Left), Some(1), true, true),
            TabMouseAction::Ignore,
            "リネーム中は入力を始め直さない"
        );
        assert_eq!(
            classify_tab_mouse(&Down(MouseButton::Middle), Some(1), true, false),
            TabMouseAction::Ignore,
            "リネーム中は閉じない"
        );
        assert_eq!(
            classify_tab_mouse(&Down(MouseButton::Left), None, false, false),
            TabMouseAction::Ignore
        );
        assert_eq!(
            classify_tab_mouse(&Down(MouseButton::Left), None, false, true),
            TabMouseAction::Ignore
        );
        assert_eq!(
            classify_tab_mouse(&Down(MouseButton::Middle), None, false, false),
            TabMouseAction::Ignore
        );
        assert_eq!(
            classify_tab_mouse(&Up(MouseButton::Middle), Some(0), false, false),
            TabMouseAction::Ignore,
            "対の Up は追跡しない"
        );
        assert_eq!(
            classify_tab_mouse(&Up(MouseButton::Left), Some(0), false, true),
            TabMouseAction::Ignore,
            "名前の入力は Down で始める"
        );
        assert_eq!(
            classify_tab_mouse(&Drag(MouseButton::Middle), Some(0), false, false),
            TabMouseAction::Ignore
        );
        assert_eq!(
            classify_tab_mouse(&Down(MouseButton::Right), Some(0), false, false),
            TabMouseAction::Ignore
        );
        assert_eq!(
            classify_tab_mouse(&ScrollUp, Some(0), false, false),
            TabMouseAction::Ignore
        );
    }

    /// ダブルクリック = 同じタブを 500ms 以内にもう一度押す。
    #[test]
    fn is_tab_double_click_needs_same_tab_within_500ms() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        assert!(is_tab_double_click(Some((t0, 2)), ms(120), Some(2)));
        assert!(
            is_tab_double_click(Some((t0, 2)), ms(500), Some(2)),
            "ちょうど 500ms は含む"
        );
        assert!(
            !is_tab_double_click(Some((t0, 2)), ms(501), Some(2)),
            "時間切れ"
        );
        assert!(
            !is_tab_double_click(Some((t0, 2)), ms(120), Some(1)),
            "別のタブ"
        );
        assert!(
            !is_tab_double_click(Some((t0, 2)), ms(120), None),
            "タブの外"
        );
        // 並べ替え・閉じる・開き直しは last を捨てる (App::move_tab / remove_tab)。
        assert!(
            !is_tab_double_click(None, ms(120), Some(2)),
            "1 回目が無い (並びが変わって捨てられた)"
        );
    }

    fn rename_press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// 入力を始めた直後は今の名前が丸ごと選ばれている: 最初の 1 文字で置き換わる。
    #[test]
    fn rename_first_char_replaces_the_whole_name() {
        let mut buf = "続きから再開".to_string();
        let mut fresh = true;
        assert_eq!(
            rename_key(&mut buf, &mut fresh, &rename_press(KeyCode::Char('A'))),
            RenameOutcome::Editing
        );
        assert_eq!(buf, "A");
        assert!(!fresh);
        rename_key(&mut buf, &mut fresh, &rename_press(KeyCode::Char('b')));
        assert_eq!(buf, "Ab", "2 文字目からは足す");
        assert_eq!(
            rename_key(&mut buf, &mut fresh, &rename_press(KeyCode::Enter)),
            RenameOutcome::Commit(Some("Ab".to_string()))
        );
    }

    /// 最初の Backspace は全部消す。そのあとは 1 文字ずつ。
    #[test]
    fn rename_backspace_clears_all_when_fresh_then_pops() {
        let mut buf = "ccnest".to_string();
        let mut fresh = true;
        rename_key(&mut buf, &mut fresh, &rename_press(KeyCode::Backspace));
        assert_eq!(buf, "");
        assert!(!fresh);
        for c in "abc".chars() {
            rename_key(&mut buf, &mut fresh, &rename_press(KeyCode::Char(c)));
        }
        rename_key(&mut buf, &mut fresh, &rename_press(KeyCode::Backspace));
        assert_eq!(buf, "ab");
    }

    /// 何も打たずに Enter → 何も変えない (自動の名前を手で付けた名前にしない)。
    #[test]
    fn rename_enter_without_typing_changes_nothing() {
        let mut buf = "自動の名前".to_string();
        let mut fresh = true;
        assert_eq!(
            rename_key(&mut buf, &mut fresh, &rename_press(KeyCode::Enter)),
            RenameOutcome::Cancel
        );
        assert_eq!(rename_finish("自動の名前", true), RenameOutcome::Cancel);
    }

    /// 空にして Enter → 手で付けた名前を外す。空白だけも同じ。前後の空白は落とす。
    #[test]
    fn rename_empty_commit_clears_the_name_and_trims() {
        let mut buf = "手で付けた名前".to_string();
        let mut fresh = true;
        rename_key(&mut buf, &mut fresh, &rename_press(KeyCode::Backspace));
        assert_eq!(
            rename_key(&mut buf, &mut fresh, &rename_press(KeyCode::Enter)),
            RenameOutcome::Commit(None)
        );
        assert_eq!(rename_finish("   ", false), RenameOutcome::Commit(None));
        assert_eq!(
            rename_finish("  Affilia P1 ", false),
            RenameOutcome::Commit(Some("Affilia P1".to_string()))
        );
    }

    /// Esc は取り消し。Ctrl / Alt 付きの文字と矢印などは入力に入れない。
    #[test]
    fn rename_esc_cancels_and_modified_chars_are_ignored() {
        let mut buf = "x".to_string();
        let mut fresh = false;
        assert_eq!(
            rename_key(&mut buf, &mut fresh, &rename_press(KeyCode::Esc)),
            RenameOutcome::Cancel
        );
        let ctrl_a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL);
        let alt_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT);
        let mut fresh = true;
        assert_eq!(
            rename_key(&mut buf, &mut fresh, &ctrl_a),
            RenameOutcome::Editing
        );
        rename_key(&mut buf, &mut fresh, &alt_f);
        rename_key(&mut buf, &mut fresh, &rename_press(KeyCode::Left));
        assert_eq!(buf, "x");
        assert!(fresh, "入力に入らないキーでは選ばれた状態のまま");
        // Shift 付き (大文字・記号) は入る。
        let shift_a = KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT);
        rename_key(&mut buf, &mut fresh, &shift_a);
        assert_eq!(buf, "A");
    }

    /// 貼り付けは最初の空でない 1 行だけ・制御文字は落とす・選ばれた状態なら置き換え。
    #[test]
    fn rename_paste_takes_first_line_and_replaces_when_fresh() {
        let mut buf = "old".to_string();
        let mut fresh = true;
        rename_paste(&mut buf, &mut fresh, "\r\n決済まわり\r\n2 行目\n");
        assert_eq!(buf, "決済まわり");
        assert!(!fresh);
        rename_paste(&mut buf, &mut fresh, "\tの\u{1b}続き");
        assert_eq!(
            buf, "決済まわりの続き",
            "2 回目からは足す・タブと ESC は落とす"
        );
    }

    /// 入れる文字の無い貼り付け (改行だけ) は何もしない。
    #[test]
    fn rename_paste_of_blank_text_keeps_the_selection() {
        let mut buf = "old".to_string();
        let mut fresh = true;
        rename_paste(&mut buf, &mut fresh, "\r\n  \n");
        assert_eq!(buf, "old");
        assert!(fresh);
    }

    /// normalize_range は行優先・同一行では col 昇順に並べ替える (i64 版)。
    #[test]
    fn normalize_range_orders_row_major() {
        assert_eq!(normalize_range((5, 10), (2, 3)), ((2, 3), (5, 10)));
        assert_eq!(normalize_range((7, 4), (1, 4)), ((1, 4), (7, 4)));
        assert_eq!(normalize_range((0, 2), (9, 8)), ((0, 2), (9, 8)));
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

    const DOC_URL: &str =
        "https://docs.google.com/document/d/1QZlfxaJs8FVooN-kZVKeb8lnJBc86sHAPZk9dFEMe_A/edit";

    /// ユーザー報告 (2026-10-31): Claude Code が URL を右端で自分で割り
    /// (CR + CSI C + CSI B)、続きを字下げ 2 桁で置く。row_wrapped は立たないが、
    /// どちらの行を Ctrl+クリックしても URL 全体を開く。直後の `)・リンク` は含めない。
    #[test]
    fn url_at_cell_joins_claude_app_wrapped_url() {
        let mut parser = vt100::Parser::new(6, 48, 100);
        // "  原本 (" = 8 桁。URL の先頭 39 文字で 47 桁目まで埋める。
        let (head, tail) = DOC_URL.split_at(39);
        parser
            .process(format!("  原本 ({head}{CLAUDE_NEXT_ROW}{tail})・リンクは開ける").as_bytes());
        assert!(!parser.screen().row_wrapped(0), "前提: アプリの改行");
        let got = url_at_cell_in_screen(parser.screen(), 20, 0);
        assert_eq!(got.as_deref(), Some(DOC_URL));
        let got = url_at_cell_in_screen(parser.screen(), 5, 1);
        assert_eq!(got.as_deref(), Some(DOC_URL));
    }

    /// 最終列まで埋めて割ると ConPTY は自動折り返しで次行へ進み、字下げを空白で
    /// 書く。その空白は URL に入れない。
    #[test]
    fn url_at_cell_drops_indent_after_autowrap() {
        let mut parser = vt100::Parser::new(6, 48, 100);
        let (head, tail) = DOC_URL.split_at(46);
        parser.process(format!("  {head}  {tail} です").as_bytes());
        assert!(parser.screen().row_wrapped(0), "前提: 自動折り返し");
        let got = url_at_cell_in_screen(parser.screen(), 10, 0);
        assert_eq!(got.as_deref(), Some(DOC_URL));
        let got = url_at_cell_in_screen(parser.screen(), 4, 1);
        assert_eq!(got.as_deref(), Some(DOC_URL));
    }

    /// URL の直後に空白なしで日本語が続いても、URL は日本語の手前で終わる。
    #[test]
    fn url_at_cell_stops_before_japanese() {
        let mut parser = vt100::Parser::new(4, 48, 100);
        parser.process("  原本 (https://x.com/a)・リンク".as_bytes());
        let got = url_at_cell_in_screen(parser.screen(), 12, 0);
        assert_eq!(got.as_deref(), Some("https://x.com/a"));
    }

    /// URL を 1 行ずつ並べた一覧は、右端まで届いていてもつながない。
    #[test]
    fn url_at_cell_does_not_join_url_list() {
        let mut parser = vt100::Parser::new(4, 48, 100);
        let first = format!("https://example.com/{}", "a".repeat(27)); // 47 桁
        parser.process(format!("{first}\r\nhttps://example.com/b").as_bytes());
        let got = url_at_cell_in_screen(parser.screen(), 5, 0);
        assert_eq!(got.as_deref(), Some(first.as_str()));
        let got = url_at_cell_in_screen(parser.screen(), 5, 1);
        assert_eq!(got.as_deref(), Some("https://example.com/b"));
    }

    /// 右端に届かない短い URL の行は、次の行とつながない。
    #[test]
    fn url_at_cell_does_not_join_short_row() {
        let mut parser = vt100::Parser::new(4, 48, 100);
        parser.process(b"  https://x.com/a\r\n  next");
        let got = url_at_cell_in_screen(parser.screen(), 5, 0);
        assert_eq!(got.as_deref(), Some("https://x.com/a"));
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

    // -- pending_arrow_expired --

    /// 窓ちょうど (経過 == PAIR_WINDOW) はまだフラッシュしない。classify_arrow の
    /// Drop 窓が `<= pair_window` なので、境界を厳密不等号で相補させ隙間を作らない。
    #[test]
    fn pending_arrow_expired_false_at_exact_window() {
        let now = Instant::now();
        assert!(!pending_arrow_expired(now - W, W, now));
    }

    /// 窓超過で実キー確定フラッシュ。
    #[test]
    fn pending_arrow_expired_true_past_window() {
        let now = Instant::now();
        let deferred = now - (W + Duration::from_millis(1));
        assert!(pending_arrow_expired(deferred, W, now));
    }

    /// now < deferred_at (計測順序の逆転) でも panic せず false (saturating)。
    #[test]
    fn pending_arrow_expired_saturates_on_clock_skew() {
        let now = Instant::now();
        assert!(!pending_arrow_expired(
            now + Duration::from_millis(10),
            W,
            now
        ));
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

    // Shift / Ctrl / Alt 付きの Enter は改行 (ESC CR)、素の Enter だけ送信 (CR)。
    // Alt は WT の sendInput "ESC CR" が ConPTY で Alt+Enter に合成されて届く経路。
    #[test]
    fn key_to_bytes_enter_modifiers_insert_newline() {
        for mods in [
            KeyModifiers::SHIFT,
            KeyModifiers::CONTROL,
            KeyModifiers::ALT,
        ] {
            let k = KeyEvent::new(KeyCode::Enter, mods);
            assert_eq!(key_to_bytes(&k, false), b"\x1b\r", "{mods:?}");
            assert_eq!(key_to_bytes(&k, true), b"\x1b\r", "{mods:?}");
        }
        assert_eq!(key_to_bytes(&plain(KeyCode::Enter), false), b"\r");
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
// ver0.5 - 2026-08-11 - Move the deferred phantom-arrow flush from before
//                       event::poll to immediately after the queue drain
//                       (process_batch), restoring the invariant that a queued
//                       paired wheel always cancels its phantom before any
//                       flush. The old placement leaked \x1bOA to Claude
//                       (prompt history) whenever a loop iteration stalled past
//                       PAIR_WINDOW — regressed by the heavier 2s tick of
//                       v0.1.7/0.1.8. Flush timing extracted into pure
//                       pending_arrow_expired() (strict > to complement
//                       classify_arrow's <= Drop window; saturating on clock
//                       skew). CCNEST_INPUT_TRACE now logs each flush as
//                       "pending_arrow_flush Key(...)" (leak signature for
//                       E2E). Sidebar git status walk is skipped while hidden,
//                       with an immediate refresh on the hidden→visible edge.
// ver0.6 - 2026-09-06 - Event loop is now woken by PTY output: an input pump
//                       thread reads crossterm events (read + poll(0) drain,
//                       one LoopMsg::Input per drain) and each pane's reader
//                       sends LoopMsg::Output, so run_event_loop waits on a
//                       single recv_timeout instead of event::poll(30ms) —
//                       echoes no longer sit until a timer tick (31–47ms on
//                       Windows' 15.6ms timer). Draw only when dirty (output
//                       frames coalesced at 8ms, input frames immediate).
//                       should_extend_burst() limits the 5ms paste-burst wait
//                       to drains holding >=2 presses or an Event::Paste, so a
//                       lone keystroke is forwarded immediately;
//                       extend_paste_burst() never extends the window on
//                       output notifications. sync_pane_sizes() replaces the
//                       per-frame pane.resize in the draw closure. Pending-
//                       arrow flush stays right after drain + process_batch.
//                       CCNEST_LATENCY_TRACE=1 logs per-keystroke
//                       key_write->output / output->draw / burst_wait plus
//                       "draw ..." / "output pid=..." event lines. The output
//                       frame cap is measured from the last output-driven draw
//                       so an echo right after a keystroke is drawn at once.
//                       Measured (30 injected keys): Claude pane p50/p90
//                       14.6/46.5 ms -> 9.1/12.0 ms; cmd pane 14.4/46.7 ms ->
//                       1.3/1.7 ms; idle CPU ~2% -> ~0%.
// ver0.7 - 2026-09-07 - Typing while the child is busy: a keystroke now arms
//                       app.echo_pending, and the first output-driven draw
//                       inside CCNEST_ECHO_WINDOW_MS (default 100) skips the
//                       output frame cap, so an echo is no longer delayed only
//                       while the pane is streaming. A batch made purely of
//                       Mouse(Moved) no longer marks the frame dirty (except
//                       with a context menu open or a drag selection running),
//                       and forward_or_swallow only forwards a motion report
//                       when the cell changed - with the pointer moving, draws
//                       over the same input fell 376 -> 151 and key->screen p90
//                       22 -> 17 ms. idle_wait / min_output_frame / echo_window
//                       are overridable via env (pure parse_ms_override).
// ver0.8 - 2026-09-26 - Middle-click (wheel click) on a tab closes the whole
//                       tab: pure tab_at() / classify_tab_mouse() replace the
//                       inline left-click loop, Down(Middle) -> App::close_tab,
//                       and tab_rects is cleared right after a close so a
//                       second click in the same batch cannot hit a stale
//                       index. process_batch and run_event_loop stop once
//                       app.quit is set, so no queued event or pending-arrow
//                       flush touches current_tab() with zero tabs.
//                       CCNEST_INPUT_TRACE logs "close_tab idx=N tabs_left=M".
// ver0.9 - 2026-10-04 - Copy joins rows that Claude Code hard-wrapped itself:
//                       a long path/URL split at the right edge (within
//                       APP_WRAP_EDGE_SLACK = 2 columns, measured on Claude Code
//                       v2.1.289) is copied as one line with the next row's
//                       indent removed. Rows stay apart when the next row starts
//                       a list item or a new path/URL, when its indent is
//                       shallower, or when the row's last word is not path-like.
// ver0.10 - 2026-10-06 - Start the plan-usage poller thread next to the input
//                        pump (claude::usage::spawn_usage_poller).
// ver0.11 - 2026-10-10 - Copy drops the continuation row's indent after a terminal
//                        auto-wrap too (app_wrapped_after_autowrap): when Claude
//                        Code fills a row to the last column, ConPTY continues
//                        through the terminal's own wrap (row_wrapped set) and
//                        writes the hanging indent as spaces, which used to be
//                        copied verbatim ("...247e  4b44..."). AppWrapTraits gains
//                        body_indent; the spaces are dropped only when they match
//                        the previous row's body position, the previous row ends
//                        in a splittable word, and the next row is not a list item
//                        or a new path. Measured on Claude Code v2.1.296 / 118 cols.
// ver0.12 - 2026-10-19 - The 2 s tick also calls app.spawn_next_resume() so the
//                        `--resume` sessions open one tab per tick (errors go to
//                        the status line).
// ver0.13 - 2026-10-27 - Copy of Claude Code's Japanese replies: a row that
//                        reaches the right edge joins the next row with no
//                        newline and no space when the boundary is a wide
//                        (CJK / full-width) character on either side and the
//                        next row starts exactly at the row's body column
//                        (continues_wide_prose; also after a terminal
//                        auto-wrap). Rows starting with Japanese list heads
//                        (`・` `※` `①` `1）` ...) are never joined. After the
//                        rows are decided, dedent_copied_lines removes the
//                        indent shared by all non-empty lines (the first line
//                        is left alone when the selection starts mid-row; a
//                        leading `● ` selected from column 0 counts as
//                        indent), keeping blank lines and relative indents.
// ver0.14 - 2026-10-31 - Ctrl+click URL detection joins rows with the same rule
//                        as copy (row_join: terminal soft wrap, soft wrap with an
//                        app indent, or Claude Code's own right-edge break), drops
//                        the trailing blanks / hanging indent at an app break, and
//                        ends the URL at the first non-URL character (is_url_char).
//                        CCNEST_OPEN_URL_LOG=<path> logs the URL instead of opening
//                        a browser (E2E).
// ver0.15 - 2026-10-31 - Action::ReopenClosedTab -> app.reopen_closed_tab(); a
//                        status message when there is nothing to reopen.
// ver0.16 - 2026-11-03 - Action::PasteClipboard (Ctrl+V) and the right-click
//                        Paste share paste_clipboard_to_pane(): an image on
//                        the clipboard (CF_DIB/CF_BITMAP, checked without
//                        opening it) sends `ESC v` (Alt+V) so Claude Code
//                        reads the image itself; text is bracketed-pasted;
//                        image wins when both are present.
// ver0.17 - 2026-11-08 - Tab click no longer calls mark_active_tab_seen (removed
//                        with the DoneUnseen state in the two-marker status UI).
// ver0.18 - 2026-11-08 - key_to_bytes: Alt+Enter also sends ESC CR (newline).
//                        Windows Terminal's `shift+enter → sendInput "\u001b\r"`
//                        (the /terminal-setup recipe) reaches ccnest through
//                        ConPTY as an Alt+Enter KEY_EVENT (measured with
//                        scripts/e2e/conpty-input-probe.py), which used to be
//                        forwarded as a bare CR and submitted the prompt.
// ver0.19 - 2026-11-11 - The 2 s tick calls app.accept_remote_requests() before
//                        spawn_next_resume(), so sessions requested with
//                        `ccnest --add` open as tabs in this window.
// ver0.20 - 2026-11-15 - Drag a tab to reorder: Down(Left) on a tab switches to
//                        it and sets app.tab_drag; while it is set, Drag(Left)
//                        is taken before try_forward_mouse (so a mouse-mode
//                        child never sees a press-less drag) and pure
//                        tab_drop_target() decides from the pointer column
//                        alone where the tab goes: only once the pointer is on
//                        the neighbour AND the moved tab would end up under
//                        the pointer, so tabs of different widths do not flip
//                        back and forth. tab_rects is cleared after each move.
//                        Up(Left) ends the drag. CCNEST_INPUT_TRACE logs
//                        "move_tab from=A to=B".
// ver0.21 - 2026-11-15 - Floating tab drag: Drag(Left) updates
//                        TabDrag::pointer_x and lifts the tab once the pointer
//                        leaves the pressed column; after move_tab the tabs it
//                        passed start sliding (App::slide_displaced) and
//                        Up(Left) starts the drop from the floating column
//                        (float_in_strip -> App::settle_tab). The loop redraws
//                        about every 16 ms only while a tab is sliding.
// ver0.22 - 2026-11-18 - Extend the selection to the click: Alt / Shift +
//                        Down(Left) in a pane (is_extend_click) builds the
//                        selection with the pure extended_selection() from the
//                        existing selection or, when there is none, from
//                        App::click_anchor (the last plain left click, also
//                        recorded for a press held back in a mouse-mode pane).
//                        mouse_forced_local() keeps Alt + left button local in
//                        a mouse-mode pane (Shift / Ctrl as before). The anchor
//                        is dropped when the wheel is forwarded to an
//                        alternate-screen child. fmt_event shows mouse
//                        modifiers; the trace logs "extend_selection ...".
// ver0.23 - 2026-11-21 - Double-click a tab to rename it: a second Down(Left) on
//                        the same tab within 500 ms (is_tab_double_click,
//                        App::last_tab_click) is TabMouseAction::Rename and
//                        opens the name input. The input is Explorer-style
//                        (pure rename_key / rename_finish / rename_paste ->
//                        RenameOutcome): the name starts selected, the first
//                        key replaces it, Enter without typing changes
//                        nothing, an empty name removes the hand-made name.
//                        A left click outside the edited tab commits and is
//                        swallowed (taken before try_forward_mouse). Paste
//                        and Ctrl+V go into the name while renaming. The
//                        trace logs begin_rename_tab / rename_tab.
// ver0.24 - 2026-11-21 - Working tabs are parked on the right: the 2 s tick calls
//                        app.refresh_tab_parking() right after
//                        refresh_pane_state(). A tab drag only reorders
//                        within the dragged tab's group (same_group_rects
//                        feeds tab_drop_target), and slides of tabs in the
//                        other group are cancelled after a move.
// ver0.25 - 2026-11-21 - Alt+C (Action::ToggleHoldTab) puts the front tab on
//                        hold / takes it off hold (app.toggle_hold_tab) and
//                        clears the selection. same_group_rects is generic
//                        over the group key (now Tab::group: named / parked
//                        green / on hold). The trace logs
//                        `hold_tab idx=N held=.. active=M`.
