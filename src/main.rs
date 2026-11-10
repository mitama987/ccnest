use std::backtrace::Backtrace;
use std::fs::{create_dir_all, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, SetTitle,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use ccnest::app::{folder_title, App};
use ccnest::cli::{parse_args, Cli};
use ccnest::event::run_event_loop;
use ccnest::remote;

fn main() -> Result<()> {
    // raw mode に入る前に解析する (不正な引数はエラー表示で即終了)。
    let cli = parse_args(std::env::args().skip(1))?;
    if cli.add {
        // 新しい窓は開かない。動いている窓へ依頼を送って結果を表示するだけ。
        std::process::exit(run_add(&cli));
    }
    let cwd = cli
        .cwd
        .unwrap_or_else(|| std::env::current_dir().expect("current_dir"));

    install_panic_hook();

    enable_raw_mode().context("enable_raw_mode")?;
    let mut stdout = io::stdout();
    // ウィンドウタイトル (= タスクバーの名前) を起動フォルダ名にする。
    // 未設定だと起動元 cmd の「C:\Windows\System32\cmd.exe」のままで窓を区別できない。
    execute!(
        stdout,
        SetTitle(folder_title(&cwd)),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    // Rust の io::Stdout は 1KB の LineWriter。フレーム差分がそれを超えると
    // 1KB ごとに WriteConsoleW が走るので、大きめのバッファで包む
    // (ratatui は draw の要所で flush する)。
    let backend = CrosstermBackend::new(BufWriter::with_capacity(1 << 20, stdout));
    let mut terminal = Terminal::new(backend)?;

    let app = App::with_resumes(cwd, cli.resumes)?;
    let result = run_event_loop(&mut terminal, app);

    restore_terminal();
    terminal.show_cursor().ok();

    result
}

/// `ccnest --add [--to <pid>] --resume <uuid>[=<タブ名>]...`: 動いている窓へ
/// 「このセッションをタブで開いて」を送る。終了コードは、頼んだセッションが
/// すべてその窓のタブにある (開いた／開いていた) なら 0、それ以外は 1。
fn run_add(cli: &Cli) -> i32 {
    match send_add(cli) {
        Ok((text, ok)) => {
            println!("{text}");
            i32::from(!ok)
        }
        Err(e) => {
            eprintln!("ccnest --add: {e:#}");
            1
        }
    }
}

fn send_add(cli: &Cli) -> Result<(String, bool)> {
    let env = std::env::var(remote::PID_ENV).ok();
    let pid = remote::resolve_target(cli.to, env.as_deref())?;
    let root = remote::requests_root().context("no data dir for ccnest requests")?;
    if !remote::pid_alive(pid) {
        anyhow::bail!("pid {pid} の ccnest は動いていません");
    }
    let reply = remote::send(&remote::inbox_dir(&root, pid), &cli.resumes, ADD_TIMEOUT)?;
    Ok((
        remote::format_reply(pid, &cli.resumes, &reply),
        reply.all_present(),
    ))
}

/// 窓側は 2 秒 tick で拾う。tick が重い瞬間 (git の走査など) を見込んで余裕を持つ。
const ADD_TIMEOUT: Duration = Duration::from_secs(10);

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

// Version History
// ver0.1 - 2026-09-06 - Wrap stdout in a 1 MB BufWriter so a large frame diff is
//                       not flushed to the console in 1 KB LineWriter chunks.
// ver0.2 - 2026-10-19 - Parse args with cli::parse_args (cwd + repeated
//                       `--resume <uuid>[=<title>]`) and start via
//                       App::with_resumes.
// ver0.3 - 2026-10-25 - Set the window title (taskbar name) to the start
//                       folder name (folder_title) so ccnest windows can be
//                       told apart instead of all reading "C:\Windows\...".
// ver0.4 - 2026-11-11 - `--add`: run_add() sends the `--resume` sessions to a
//                       running window (remote::send) and exits without
//                       entering raw mode; exit code 0 only when every
//                       requested session ends up as a tab there.
