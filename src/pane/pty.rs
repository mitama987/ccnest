use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, MasterPty, NativePtySystem, PtyPair, PtySize, PtySystem};

pub struct PtyHandle {
    inner: Arc<Mutex<PtyInner>>,
}

struct PtyInner {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl PtyHandle {
    pub fn spawn(cmd: CommandBuilder, parser: Arc<Mutex<vt100::Parser>>) -> Result<Self> {
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
        let writer = master.take_writer().context("take_writer")?;
        let mut reader = master.try_clone_reader().context("try_clone_reader")?;

        let inner = Arc::new(Mutex::new(PtyInner {
            master,
            writer,
            child,
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
                        carry.extend_from_slice(&buf[..n]);
                        let replies = scan_replies(&mut carry, &parser);
                        if !replies.is_empty() {
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

    pub fn kill(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            let _ = guard.child.kill();
        }
    }
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
}
