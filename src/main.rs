use std::backtrace::Backtrace;
use std::fs::{create_dir_all, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use ccnest::app::App;
use ccnest::event::run_event_loop;

fn main() -> Result<()> {
    let cwd = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().expect("current_dir"));

    install_panic_hook();

    enable_raw_mode().context("enable_raw_mode")?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    // DECRST 1007 (Alternate Scroll Mode off): alt 画面中にホスト端末
    // (Windows Terminal / conhost) がマウスホイール回転を `\x1b[A`/`\x1b[B`
    // に変換して stdin に注入する挙動を止める。これがないと EnableMouseCapture
    // で SGR マウスイベントは届くのに、同時に注入されるファントム矢印が子
    // (Claude Code) に渡り「ホイールで履歴が呼ばれる」バグになる。crossterm の
    // EnableMouseCapture は 1000/1002/1003/1006/1015h は送るが 1007 は触らない
    // ため明示的に送る。CrosstermBackend が stdout を move する前に書く必要。
    stdout.write_all(b"\x1b[?1007l")?;
    stdout.flush()?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let app = App::new(cwd)?;
    let result = run_event_loop(&mut terminal, app);

    restore_terminal();
    terminal.show_cursor().ok();

    result
}

/// Restore the terminal to a usable state. Safe to call multiple times and
/// from a panic context — every operation is best-effort and ignores errors
/// because by the time we get here the terminal may already be in a partial
/// state.
fn restore_terminal() {
    disable_raw_mode().ok();
    let mut stdout = io::stdout();
    execute!(
        stdout,
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    )
    .ok();
    // 起動時に落とした Alternate Scroll Mode をホスト既定 (alt 画面で 1007h)
    // に戻す。本関数は全操作ベストエフォート / エラー無視の契約なので `let _`。
    let _ = stdout.write_all(b"\x1b[?1007h");
    let _ = stdout.flush();
}

/// Install a panic hook that:
/// 1. Restores the terminal so the user gets a working shell back instead of
///    a frozen alternate-screen with corrupted output.
/// 2. Appends panic info + backtrace to a crash log under the OS data dir
///    (e.g. `%APPDATA%\ccnest\crash-YYYYMMDD-HHMMSS.log` on Windows) so the
///    next session has something to diagnose from.
/// 3. Delegates to the default hook so the panic is also printed to stderr.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();

        if let Some(path) = write_crash_log(info) {
            eprintln!("ccnest: crash log written to {}", path.display());
        }

        default_hook(info);
    }));
}

fn write_crash_log(info: &std::panic::PanicHookInfo<'_>) -> Option<PathBuf> {
    let dir = dirs::data_dir()?.join("ccnest");
    create_dir_all(&dir).ok()?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let path = dir.join(format!("crash-{stamp}.log"));
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    let _ = writeln!(
        file,
        "ccnest panic at {}",
        chrono::Local::now().to_rfc3339()
    );
    let _ = writeln!(file, "version: {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(file, "{info}");
    let _ = writeln!(file, "{}", Backtrace::force_capture());
    Some(path)
}
