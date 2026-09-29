//! Embedded terminal: runs `claude --resume` on a PTY and keeps a vt100
//! screen model that the UI renders as a pane. The child stays alive when the
//! user detaches (Ctrl-q) and is killed when the session/app is dropped.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};

use crate::watch::AppEvent;

pub struct PtySession {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    /// Last time the child produced output; fresh output = claude is drawing.
    last_output: Arc<Mutex<Instant>>,
}

impl PtySession {
    /// Spawn `claude --resume <id>` in `cwd` on a fresh PTY. A reader thread
    /// feeds the vt100 parser and pokes the event loop for redraws.
    pub fn spawn(
        session_id: String,
        cwd: &Path,
        rows: u16,
        cols: u16,
        tx: Sender<AppEvent>,
    ) -> anyhow::Result<Self> {
        let pty = native_pty_system().openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let mut cmd = CommandBuilder::new("claude");
        cmd.args(["--resume", &session_id]);
        cmd.cwd(cwd);
        let child = pty.slave.spawn_command(cmd)?;
        let killer = child.clone_killer();
        // The slave fd must be dropped in the parent or reads never see EOF.
        drop(pty.slave);

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        let writer = pty.master.take_writer()?;
        let mut reader = pty.master.try_clone_reader()?;
        let last_output = Arc::new(Mutex::new(Instant::now()));

        let thread_parser = parser.clone();
        let thread_last = last_output.clone();
        std::thread::spawn(move || {
            // Keep the child handle on this thread so its exit is observable.
            let mut child = child;
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break, // EOF: child closed the PTY
                    Ok(n) => {
                        thread_parser.lock().expect("parser lock").process(&buf[..n]);
                        *thread_last.lock().expect("ts lock") = Instant::now();
                        let _ = tx.send(AppEvent::Pty);
                    }
                }
            }
            let _ = child.wait();
            let _ = tx.send(AppEvent::PtyExited { id: session_id });
        });

        Ok(Self { parser, writer, master: pty.master, killer, last_output })
    }

    /// Locked vt100 parser (screen) for rendering.
    pub fn parser(&self) -> MutexGuard<'_, vt100::Parser> {
        self.parser.lock().expect("parser lock")
    }

    /// Did the child write output within `window`? (= claude is busy drawing.)
    pub fn output_within(&self, window: Duration) -> bool {
        self.last_output.lock().expect("ts lock").elapsed() < window
    }

    pub fn send_key(&mut self, key: KeyEvent) {
        if let Some(bytes) = encode_key(key) {
            let _ = self.writer.write_all(&bytes);
            let _ = self.writer.flush();
        }
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        self.parser().screen_mut().set_size(rows, cols);
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        // No orphaned claude processes silently burning tokens.
        let _ = self.killer.kill();
    }
}

/// Translate a crossterm key event into the byte sequence a terminal would
/// send. Returns None for keys with no sensible encoding.
pub fn encode_key(key: KeyEvent) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let mut buf: Vec<u8> = Vec::new();
    if alt {
        buf.push(0x1b);
    }
    match key.code {
        KeyCode::Char(c) if ctrl => {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() {
                buf.push(c as u8 - b'a' + 1);
            } else {
                return None;
            }
        }
        KeyCode::Char(c) => {
            let mut utf8 = [0u8; 4];
            buf.extend_from_slice(c.encode_utf8(&mut utf8).as_bytes());
        }
        KeyCode::Enter => buf.push(b'\r'),
        KeyCode::Tab => buf.push(b'\t'),
        KeyCode::BackTab => buf.extend_from_slice(b"\x1b[Z"),
        KeyCode::Backspace => buf.push(0x7f),
        KeyCode::Esc => buf.push(0x1b),
        KeyCode::Up => buf.extend_from_slice(b"\x1b[A"),
        KeyCode::Down => buf.extend_from_slice(b"\x1b[B"),
        KeyCode::Right => buf.extend_from_slice(b"\x1b[C"),
        KeyCode::Left => buf.extend_from_slice(b"\x1b[D"),
        KeyCode::Home => buf.extend_from_slice(b"\x1b[H"),
        KeyCode::End => buf.extend_from_slice(b"\x1b[F"),
        KeyCode::PageUp => buf.extend_from_slice(b"\x1b[5~"),
        KeyCode::PageDown => buf.extend_from_slice(b"\x1b[6~"),
        KeyCode::Delete => buf.extend_from_slice(b"\x1b[3~"),
        KeyCode::Insert => buf.extend_from_slice(b"\x1b[2~"),
        _ => return None,
    }
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn encodes_common_keys() {
        assert_eq!(encode_key(key(KeyCode::Char('a'), KeyModifiers::NONE)), Some(vec![b'a']));
        assert_eq!(encode_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL)), Some(vec![3]));
        assert_eq!(encode_key(key(KeyCode::Enter, KeyModifiers::NONE)), Some(vec![b'\r']));
        assert_eq!(encode_key(key(KeyCode::Esc, KeyModifiers::NONE)), Some(vec![0x1b]));
        assert_eq!(encode_key(key(KeyCode::Up, KeyModifiers::NONE)), Some(b"\x1b[A".to_vec()));
        // Alt prefixes ESC, unicode survives.
        assert_eq!(
            encode_key(key(KeyCode::Char('x'), KeyModifiers::ALT)),
            Some(vec![0x1b, b'x'])
        );
        assert_eq!(
            encode_key(key(KeyCode::Char('ä'), KeyModifiers::NONE)),
            Some("ä".as_bytes().to_vec())
        );
    }
}
