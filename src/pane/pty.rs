use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, MasterPty, NativePtySystem, PtyPair, PtySize, PtySystem};

use crate::wake::{OutputStamp, OutputWaker};

/// PTY reader スレッドが出力を parser へ反映した直後に呼ぶフック一式。
/// reader はロックの外で呼ぶ (parser ロックを持ったまま他スレッドを起こさない)。
#[derive(Clone, Debug, Default)]
pub struct ReaderHooks {
    /// 最終出力時刻 (`CCNEST_LATENCY_TRACE` 用)。
    pub stamp: OutputStamp,
    /// イベントループへの「出力あり」通知。None なら通知しない (probe 用)。
    pub waker: Option<OutputWaker>,
}

pub struct PtyHandle {
    inner: Arc<Mutex<PtyInner>>,
}

struct PtyInner {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    /// 子プロセスとその子孫をまとめた Job Object (Windows)。`kill` で木ごと
    /// 終了させ、ccnest 自体が落ちてもハンドルが閉じれば OS が全部終わらせる。
    /// 割り当てに失敗した (None) ときは `kill` が子孫を列挙して個別に止める。
    tree: Option<tree::ProcessTree>,
}

impl PtyHandle {
    pub fn spawn(
        cmd: CommandBuilder,
        parser: Arc<Mutex<vt100::Parser>>,
        hooks: ReaderHooks,
    ) -> Result<Self> {
        let pty_system = NativePtySystem::default();
        let PtyPair { master, slave } = pty_system
            .openpty(PtySize {
                rows: 40,
                cols: 140,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("openpty")?;

        let child = slave.spawn_command(cmd).context("spawn_command")?;
        drop(slave);
        // CreateProcessW が返った直後 = 子はまだローダ初期化中で孫を作れていないので、
        // ここで Job に入れれば以後に生まれる孫 (シム → claude.exe) も全部 Job に属する。
        let tree = child.process_id().and_then(tree::ProcessTree::adopt);
        let writer = master.take_writer().context("take_writer")?;
        let mut reader = master.try_clone_reader().context("try_clone_reader")?;

        let inner = Arc::new(Mutex::new(PtyInner {
            master,
            writer,
            child,
            tree,
        }));

        let inner_r = Arc::clone(&inner);
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            let mut carry: Vec<u8> = Vec::new();
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Ok(mut p) = parser.lock() {
                            p.process(&buf[..n]);
                        }
                        hooks.stamp.mark();
                        if let Some(w) = &hooks.waker {
                            w.notify();
                        }
                        dump_pty_bytes("out", &buf[..n]);
                        carry.extend_from_slice(&buf[..n]);
                        let replies = scan_replies(&mut carry, &parser);
                        if !replies.is_empty() {
                            dump_pty_bytes("reply", &replies);
                            if let Ok(mut g) = inner_r.lock() {
                                let _ = g.writer.write_all(&replies);
                                let _ = g.writer.flush();
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self { inner })
    }

    pub fn write(&self, data: &[u8]) {
        if let Ok(mut guard) = self.inner.lock() {
            let _ = guard.writer.write_all(data);
            let _ = guard.writer.flush();
        }
    }

    pub fn resize(&self, rows: u16, cols: u16) {
        if let Ok(guard) = self.inner.lock() {
            let _ = guard.master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }

    /// 子プロセスを子孫ごと終了させる。portable-pty の `Child::kill` は直下の子
    /// (CCNEST_CLAUDE_BIN のシム) を TerminateProcess するだけで、孫の claude.exe
    /// は ConPTY を失ったまま生き残っていた (2026-10-21: 閉じたタブ 19 本分・約
    /// 5.5GB が孤児として残存)。Job があれば TerminateJobObject で木ごと、無ければ
    /// 子孫を列挙して葉から順に止めてから子を kill する。
    pub fn kill(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            match &guard.tree {
                Some(t) => t.kill(),
                None => {
                    if let Some(pid) = guard.child.process_id() {
                        tree::kill_descendants(pid);
                    }
                }
            }
            let _ = guard.child.kill();
        }
    }

    /// 直下の子プロセスの PID (テスト・診断用)。
    #[cfg(test)]
    pub fn child_pid(&self) -> Option<u32> {
        self.inner.lock().ok().and_then(|g| g.child.process_id())
    }
}

/// ペインの子プロセス木の管理 (Windows は Job Object、他 OS は何もしない)。
pub mod tree {
    #[cfg(windows)]
    mod imp {
        use std::mem::{size_of, zeroed};

        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        };
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        use windows_sys::Win32::System::Threading::{
            OpenProcess, TerminateProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
        };

        /// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 付きの Job Object。最後のハンドルが
        /// 閉じた時点 (= この値の Drop、または ccnest プロセスの終了) で所属プロセスが
        /// 全部終了する。Windows 8 以降は入れ子の Job が許されるので、ccnest 自身が
        /// 別の Job (Windows Terminal 等) に属していても割り当てられる。
        pub struct ProcessTree {
            job: HANDLE,
        }

        // HANDLE は生ポインタ型だが Job ハンドルはスレッドをまたいで使える。
        unsafe impl Send for ProcessTree {}

        impl ProcessTree {
            /// `pid` を新しい Job に入れる。失敗 (権限・Job 作成不可) は None。
            pub fn adopt(pid: u32) -> Option<Self> {
                unsafe {
                    let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                    if job == 0 {
                        return None;
                    }
                    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
                    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                    let ok = SetInformationJobObject(
                        job,
                        JobObjectExtendedLimitInformation,
                        &info as *const _ as *const std::ffi::c_void,
                        size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                    );
                    if ok == 0 {
                        CloseHandle(job);
                        return None;
                    }
                    let proc = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
                    if proc == 0 {
                        CloseHandle(job);
                        return None;
                    }
                    let assigned = AssignProcessToJobObject(job, proc);
                    CloseHandle(proc);
                    if assigned == 0 {
                        CloseHandle(job);
                        return None;
                    }
                    Some(Self { job })
                }
            }

            /// Job に属する全プロセスを即時終了させる。
            pub fn kill(&self) {
                unsafe {
                    TerminateJobObject(self.job, 1);
                }
            }
        }

        impl Drop for ProcessTree {
            fn drop(&mut self) {
                unsafe {
                    CloseHandle(self.job);
                }
            }
        }

        /// 現在の全プロセスの (pid, 親 pid) 一覧。
        pub fn process_table() -> Vec<(u32, u32)> {
            let mut out = Vec::new();
            unsafe {
                let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
                if snap == INVALID_HANDLE_VALUE {
                    return out;
                }
                let mut e: PROCESSENTRY32W = zeroed();
                e.dwSize = size_of::<PROCESSENTRY32W>() as u32;
                if Process32FirstW(snap, &mut e) != 0 {
                    loop {
                        out.push((e.th32ProcessID, e.th32ParentProcessID));
                        if Process32NextW(snap, &mut e) == 0 {
                            break;
                        }
                    }
                }
                CloseHandle(snap);
            }
            out
        }

        /// `root` の子孫 PID を深さ優先 (親より先に子) の順で返す。
        pub fn descendants(root: u32) -> Vec<u32> {
            super::descendants_in(&process_table(), root)
        }

        /// Job に入れられなかったときの代替: 子孫を列挙して個別に TerminateProcess。
        pub fn kill_descendants(root: u32) {
            for pid in descendants(root) {
                unsafe {
                    let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
                    if h != 0 {
                        TerminateProcess(h, 1);
                        CloseHandle(h);
                    }
                }
            }
        }
    }

    #[cfg(not(windows))]
    mod imp {
        /// 非 Windows では未対応 (常に None)。portable-pty の kill に任せる。
        pub struct ProcessTree;
        impl ProcessTree {
            pub fn adopt(_pid: u32) -> Option<Self> {
                None
            }
            pub fn kill(&self) {}
        }
        pub fn descendants(_root: u32) -> Vec<u32> {
            Vec::new()
        }
        pub fn kill_descendants(_root: u32) {}
    }

    pub use imp::*;

    /// 純粋関数: (pid, 親 pid) の表から `root` の子孫 PID を返す (親より先に子が
    /// 来る順)。PID 0 (System Idle) は自分が自分の親なので `pid != p` で除く。
    pub fn descendants_in(table: &[(u32, u32)], root: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut stack = vec![root];
        while let Some(p) = stack.pop() {
            for &(pid, ppid) in table {
                if ppid == p && pid != p && !out.contains(&pid) {
                    out.push(pid);
                    stack.push(pid);
                }
            }
        }
        out
    }
}

/// `CCNEST_PTY_DUMP` の値から決まる、reader の生バイト記録のしかた (開発用)。
#[derive(Debug, Clone, PartialEq, Eq)]
enum PtyDumpMode {
    /// 未設定。何もしない。
    Off,
    /// `CCNEST_PTY_DUMP=1` (空・`true`・`on` も同じ)。`%APPDATA%\ccnest\pty-dump.log`
    /// に、時刻・種別・長さ付きで制御文字を `<ESC>` などに置き換えて 1 行ずつ追記する。
    EscapedLog,
    /// `CCNEST_PTY_DUMP=<path>`。全ペインの ConPTY 出力の生バイトをそのまま
    /// `<path>` に追記する (テストの fixture に起こしやすい)。DSR 等への返信は
    /// 子への入力なので書かない。
    RawFile(std::path::PathBuf),
}

impl PtyDumpMode {
    fn from_env_value(value: Option<&str>) -> Self {
        match value {
            None => Self::Off,
            Some(v) if matches!(v.trim(), "" | "1" | "true" | "on") => Self::EscapedLog,
            Some(v) => Self::RawFile(std::path::PathBuf::from(v)),
        }
    }

    fn write(&self, kind: &str, bytes: &[u8]) {
        match self {
            Self::Off => {}
            Self::RawFile(path) => {
                if kind != "out" {
                    return;
                }
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                {
                    let _ = f.write_all(bytes);
                }
            }
            Self::EscapedLog => {
                let Some(base) = dirs::data_dir() else {
                    return;
                };
                let path = base.join("ccnest").join("pty-dump.log");
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                {
                    let ts = chrono::Local::now().format("%H:%M:%S%.3f");
                    let esc: String = bytes
                        .iter()
                        .map(|b| match *b {
                            0x1b => String::from("<ESC>"),
                            0x0a => String::from("<LF>"),
                            0x0d => String::from("<CR>"),
                            0x20..=0x7e => (*b as char).to_string(),
                            other => format!("<{other:02x}>"),
                        })
                        .collect();
                    let _ = writeln!(f, "{ts} {kind} len={} {}", bytes.len(), esc);
                }
            }
        }
    }
}

/// `CCNEST_PTY_DUMP` が設定されているときだけ、reader が受け取った生バイト
/// (と DSR 等への返信) を記録する診断用 (`PtyDumpMode`)。未設定なら OnceLock
/// 判定のみでゼロコスト。
fn dump_pty_bytes(kind: &str, bytes: &[u8]) {
    use std::sync::OnceLock;
    static MODE: OnceLock<PtyDumpMode> = OnceLock::new();
    MODE.get_or_init(|| {
        PtyDumpMode::from_env_value(std::env::var("CCNEST_PTY_DUMP").ok().as_deref())
    })
    .write(kind, bytes);
}

/// Look for terminal queries in recently-received bytes and return the bytes
/// that should be echoed back on stdin. Queries handled:
/// - `ESC [ 5 n`   → `ESC [ 0 n`          (device status OK)
/// - `ESC [ 6 n`   → `ESC [ row ; col R`  (cursor position)
/// - `ESC [ c`     → `ESC [ ? 1 ; 2 c`    (primary device attributes, VT100+)
/// - `ESC [ > c`   → `ESC [ > 0 ; 0 ; 0 c` (secondary device attributes)
///
/// Consumes the matched bytes from `carry`; unparsed trailing bytes remain for
/// the next chunk. On Windows ConPTY the host application (= us) is expected
/// to answer these; without an answer, Claude Code exits immediately.
fn scan_replies(carry: &mut Vec<u8>, parser: &Arc<Mutex<vt100::Parser>>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut keep_from = 0;
    while i < carry.len() {
        if carry[i] != 0x1b {
            i += 1;
            keep_from = i;
            continue;
        }
        // ESC seen — try to parse a CSI sequence.
        if i + 1 >= carry.len() {
            break; // need more bytes
        }
        if carry[i + 1] != b'[' {
            i += 1;
            keep_from = i;
            continue;
        }
        // CSI: read intermediates until a final byte in 0x40..=0x7E.
        let mut j = i + 2;
        while j < carry.len() && !(0x40..=0x7e).contains(&carry[j]) {
            j += 1;
        }
        if j >= carry.len() {
            break; // incomplete sequence
        }
        let params = &carry[i + 2..j];
        let fin = carry[j];
        match (params, fin) {
            (b"5", b'n') => out.extend_from_slice(b"\x1b[0n"),
            (b"6", b'n') => {
                let (row, col) = if let Ok(p) = parser.lock() {
                    let (r, c) = p.screen().cursor_position();
                    (r as usize + 1, c as usize + 1)
                } else {
                    (1, 1)
                };
                out.extend_from_slice(format!("\x1b[{row};{col}R").as_bytes());
            }
            (b"", b'c') => out.extend_from_slice(b"\x1b[?1;2c"),
            (b">", b'c') => out.extend_from_slice(b"\x1b[>0;0;0c"),
            _ => {}
        }
        i = j + 1;
        keep_from = i;
    }
    // Drop everything we've processed; keep the tail (partial sequence) for
    // the next chunk.
    carry.drain(..keep_from);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pty_dump_mode_is_off_when_unset() {
        assert_eq!(PtyDumpMode::from_env_value(None), PtyDumpMode::Off);
    }

    #[test]
    fn pty_dump_mode_keeps_escaped_log_for_flag_values() {
        for v in ["", "1", "true", "on"] {
            assert_eq!(
                PtyDumpMode::from_env_value(Some(v)),
                PtyDumpMode::EscapedLog,
                "value: {v:?}"
            );
        }
    }

    #[test]
    fn pty_dump_mode_treats_other_values_as_raw_file_path() {
        assert_eq!(
            PtyDumpMode::from_env_value(Some("C:\\tmp\\pane.bin")),
            PtyDumpMode::RawFile(std::path::PathBuf::from("C:\\tmp\\pane.bin"))
        );
    }

    #[test]
    fn pty_dump_raw_file_appends_bytes_verbatim() {
        let dir = std::env::temp_dir().join(format!("ccnest-dump-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("raw.bin");
        let _ = std::fs::remove_file(&path);
        let mode = PtyDumpMode::RawFile(path.clone());
        mode.write("out", b"\x1b[11;1H\xe2\x97\x8f ");
        mode.write("out", "日本語\r\n".as_bytes());
        mode.write("reply", b"\x1b[1;1R");
        let got = std::fs::read(&path).expect("dump file");
        let mut want = b"\x1b[11;1H\xe2\x97\x8f ".to_vec();
        want.extend_from_slice("日本語\r\n".as_bytes());
        assert_eq!(got, want, "raw dump holds only the output bytes, unescaped");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn responds_to_dsr_6n() {
        let parser = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 1000)));
        let mut carry: Vec<u8> = b"\x1b[6n".to_vec();
        let reply = scan_replies(&mut carry, &parser);
        assert_eq!(reply, b"\x1b[1;1R");
        assert!(carry.is_empty());
    }

    #[test]
    fn responds_to_dsr_5n() {
        let parser = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 1000)));
        let mut carry: Vec<u8> = b"hello\x1b[5nworld".to_vec();
        let reply = scan_replies(&mut carry, &parser);
        assert_eq!(reply, b"\x1b[0n");
    }

    #[test]
    fn responds_to_primary_da() {
        let parser = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 1000)));
        let mut carry: Vec<u8> = b"\x1b[c".to_vec();
        let reply = scan_replies(&mut carry, &parser);
        assert_eq!(reply, b"\x1b[?1;2c");
    }

    #[test]
    fn partial_sequence_is_kept() {
        let parser = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 1000)));
        let mut carry: Vec<u8> = b"\x1b[".to_vec();
        let reply = scan_replies(&mut carry, &parser);
        assert!(reply.is_empty());
        assert_eq!(carry, b"\x1b[");
    }

    #[test]
    fn unknown_sequence_is_consumed_without_reply() {
        let parser = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 1000)));
        let mut carry: Vec<u8> = b"\x1b[1;31mHello".to_vec();
        let reply = scan_replies(&mut carry, &parser);
        assert!(reply.is_empty());
        // All processed bytes (both the SGR and the literals) are drained,
        // because vt100 has already consumed them upstream.
        assert!(carry.is_empty());
    }

    #[test]
    fn incomplete_trailing_csi_is_preserved() {
        let parser = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 1000)));
        let mut carry: Vec<u8> = b"done\x1b[".to_vec();
        let reply = scan_replies(&mut carry, &parser);
        assert!(reply.is_empty());
        // Literal "done" is drained but the partial "\x1b[" is kept for the
        // next chunk so a DSR split across reads still gets answered.
        assert_eq!(carry, b"\x1b[");
    }

    #[test]
    fn descendants_in_walks_children_before_parents_and_skips_self_parent() {
        // 10 -> 20 -> 30, 10 -> 21; 0 は自分が自分の親 (System Idle)。99 は無関係。
        let table = [(0, 0), (10, 1), (20, 10), (21, 10), (30, 20), (99, 5)];
        let d = tree::descendants_in(&table, 10);
        assert_eq!(d.len(), 3);
        assert!(d.contains(&20) && d.contains(&21) && d.contains(&30));
        assert!(d.iter().position(|&p| p == 30) > d.iter().position(|&p| p == 20));
        assert!(tree::descendants_in(&table, 0).is_empty());
        assert!(tree::descendants_in(&table, 30).is_empty());
    }

    /// `pids` のどれも生きていなくなるまで最大 `secs` 秒待つ。残った PID を返す。
    #[cfg(windows)]
    fn wait_gone(pids: &[u32], secs: u64) -> Vec<u32> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        loop {
            let live: Vec<u32> = tree::process_table().iter().map(|&(p, _)| p).collect();
            let left: Vec<u32> = pids.iter().copied().filter(|p| live.contains(p)).collect();
            if left.is_empty() || std::time::Instant::now() > deadline {
                return left;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    /// 子孫が `want` 個以上になるまで最大 `secs` 秒待つ。
    #[cfg(windows)]
    fn wait_descendants(root: u32, want: usize, secs: u64) -> Vec<u32> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        loop {
            let d = tree::descendants(root);
            if d.len() >= want || std::time::Instant::now() > deadline {
                return d;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    /// タブを閉じる経路の再現: ConPTY で cmd → cmd → ping の 3 段を起こし、
    /// `PtyHandle::kill` で孫 (ping) まで消えることを確かめる。修正前は直下の
    /// cmd だけ死んで孫が残った (= 閉じたタブの claude.exe が孤児になる症状)。
    #[cfg(windows)]
    #[test]
    fn kill_terminates_grandchildren_through_conpty() {
        let parser = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 1000)));
        let mut cmd = CommandBuilder::new("cmd.exe");
        cmd.args(["/c", "cmd.exe /c ping -n 60 127.0.0.1"]);
        let h = PtyHandle::spawn(cmd, parser, ReaderHooks::default()).expect("spawn");
        let root = h.child_pid().expect("child pid");
        let kids = wait_descendants(root, 2, 5);
        assert!(
            kids.len() >= 2,
            "expected cmd -> ping under the pane child, got {kids:?}"
        );
        h.kill();
        let mut all = kids.clone();
        all.push(root);
        let left = wait_gone(&all, 3);
        assert!(left.is_empty(), "still alive after kill: {left:?}");
    }

    /// Job に入れられなかったときの代替経路 (`kill_descendants`) 単体。
    #[cfg(windows)]
    #[test]
    fn kill_descendants_fallback_terminates_tree() {
        use std::process::{Command, Stdio};
        let mut child = Command::new("cmd.exe")
            .args(["/c", "cmd.exe /c ping -n 60 127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        let root = child.id();
        let kids = wait_descendants(root, 2, 5);
        assert!(kids.len() >= 2, "expected cmd -> ping, got {kids:?}");
        tree::kill_descendants(root);
        let left = wait_gone(&kids, 3);
        let _ = child.kill();
        let _ = child.wait();
        assert!(left.is_empty(), "descendants still alive: {left:?}");
    }
}

// Version History
// ver0.3 - 2026-10-27 - CCNEST_PTY_DUMP=<path> appends each pane's raw ConPTY
//                       output bytes, unescaped, to <path> (for test
//                       fixtures). `1` / empty / `true` / `on` keep the escaped
//                       %APPDATA%\ccnest\pty-dump.log. Unset stays a OnceLock
//                       check only (PtyDumpMode).
// ver0.2 - 2026-10-21 - Child processes are placed in a Job Object
//                       (KILL_ON_JOB_CLOSE) right after spawn, so kill()
//                       (close tab / Ctrl+W / respawn as shell) and ccnest's
//                       own exit terminate the whole tree, including the
//                       claude.exe grandchild behind the CCNEST_CLAUDE_BIN
//                       shim. Fallback when the Job cannot be created: walk
//                       the Toolhelp process table and TerminateProcess every
//                       descendant. Before this, closed tabs left claude.exe
//                       orphans (19 of them, ~5.5 GB, found on 2026-10-21).
// ver0.1 - 2026-09-06 - ReaderHooks (OutputStamp + optional OutputWaker): the
//                       reader thread marks the output time and wakes the
//                       event loop right after parser.process(), outside the
//                       parser lock. CCNEST_PTY_DUMP=1 appends the raw bytes
//                       (and DSR replies) to %APPDATA%\ccnest\pty-dump.log.
