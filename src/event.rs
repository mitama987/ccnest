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
    let idle_wait = Duration::from_millis(30);
    let auto_scroll_interval = Duration::from_millis(30);
    // 出力駆動の再描画の最短間隔。ストリーミング中に 4KB チャンクごとに描かず、
    // 最初のチャンクは即描き、以降はこの間隔で束ねる。入力起因の変化はこの
    // 制限を受けない (打鍵の反映を待たせない)。
    let min_output_frame = Duration::from_millis(8);
    let mut last_refresh = Instant::now();
    let mut last_auto_scroll = Instant::now();
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

    // dirty: 次の周で描画が必要。input_dirty: その原因に入力/操作が含まれる
    // (frame cap を待たずに描く)。初回は必ず描く。
    let mut dirty = true;
    let mut input_dirty = true;
    let mut last_draw = Instant::now() - min_output_frame;

    while !app.quit {
        // 選択の前提 (ペイン存在 / alt 画面状態) が崩れていたら描画前に破棄する。
        if validate_selection(&mut app) {
            dirty = true;
            input_dirty = true;
        }

        if dirty && (input_dirty || last_draw.elapsed() >= min_output_frame) {
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
            last_draw = Instant::now();
            dirty = false;
            input_dirty = false;
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

        // 入力 or PTY 出力 or タイムアウトを待つ。出力起因で dirty のときは
        // frame cap の残りだけ待ち、それ以外はアイドル上限まで待つ。
        let wait = if dirty {
            min_output_frame.saturating_sub(last_draw.elapsed())
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
            process_batch(
                &mut app,
                batch,
                &pane_rects,
                sidebar_file_rect,
                &tab_rects,
                &mut menu_rect,
            )?;
            dirty = true;
            input_dirty = true;
        }
        if output_seen {
            dirty = true;
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
            if app.pane_visible(pid) {
                *output_seen = true;
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
    tab_rects: &[(Rect, usize)],
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
    tab_rects: &[(Rect, usize)],
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
                // ペインリサイズは pty 再フローで行の折り返しが変わり、選択の
                // 座標が指す内容とズレるため破棄する。
                app.selection = None;
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
                        // 未閲覧完了 (マゼンタ) はクリックで開いた瞬間に消す。
                        app.mark_active_tab_seen();
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

    // 縦ホイールをここまで通過した = alt 画面の子へ転送される。子は自前で内容を
    // 動かす (フル再描画) ため ccnest からは追跡不能で、ローカル選択は内容から
    // 剥離して画面に取り残される。転送前に破棄する (any-key-clears と同じ哲学)。
    if mouse_event_reserved_for_local_scrollback(me)
        && app.selection.as_ref().is_some_and(|s| s.pane_id == pid)
    {
        app.selection = None;
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
            // OS クリップボードを読み、bracketed-paste でペインへ流す。
            let text = arboard::Clipboard::new()
                .ok()
                .and_then(|mut cb| cb.get_text().ok());
            if let Some(t) = text.filter(|t| !t.is_empty()) {
                paste_to_pane(app, pid, &t);
            }
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

    // (text, wrapped_to_next): wrapped_to_next が true の行は次行と
    // ソフトラップで連結しているので、コピー時に '\n' を挟まず、末尾の
    // trim_end も行わない (URL 末尾文字が削れないように)。
    let mut rows: Vec<(String, bool)> = Vec::new();
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

    parser.set_scrollback(saved_scrollback);

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
//                       key_write->output / output->draw / burst_wait.
