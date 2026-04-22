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
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("openpty")?;

        let child = slave.spawn_command(cmd).context("spawn_command")?;
        drop(slave);
        let writer = master.take_writer().context("take_writer")?;
        let mut reader = master.try_clone_reader().context("try_clone_reader")?;

        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Ok(mut p) = parser.lock() {
                            p.process(&buf[..n]);
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let inner = PtyInner {
            master,
            writer,
            child,
        };
        Ok(Self {
            inner: Arc::new(Mutex::new(inner)),
        })
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
