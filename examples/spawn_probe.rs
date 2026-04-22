//! Smoke probe: spawn claude via our PtyHandle (so DSR replies are
//! wired up), read its output for 4s, kill, and print a summary. Used to
//! verify ConPTY + claude interaction end-to-end without the full TUI.
//!
//! Run with: `cargo run --example spawn_probe`

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use ccnest::claude::launcher::spawn_claude;
use uuid::Uuid;

fn main() {
    let parser = Arc::new(Mutex::new(vt100::Parser::new(40, 140, 2000)));
    let session_id = Uuid::new_v4();
    let cwd = std::env::current_dir().expect("cwd");

    let (pty, cmd_label, claude_running) =
        spawn_claude(&cwd, session_id, Arc::clone(&parser)).expect("spawn_claude");
    println!("spawned: {cmd_label} (claude_running={claude_running})");

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        thread::sleep(Duration::from_millis(200));
    }

    pty.kill();
    thread::sleep(Duration::from_millis(300));

    let g = parser.lock().unwrap();
    let screen = g.screen();
    let contents = screen.contents();
    let visible_len = contents.chars().filter(|c| !c.is_whitespace()).count();
    println!("visible (non-ws) chars on screen: {visible_len}");
    println!("session_id: {session_id}");
    println!("--- screen dump (first 40 lines) ---");
    for (i, line) in contents.lines().take(40).enumerate() {
        println!("{:>2}: {}", i + 1, line);
    }
}
