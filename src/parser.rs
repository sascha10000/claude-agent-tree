//! Streaming JSONL reader with offset tracking, tolerant of live-written files.
//!
//! Truncated-EOF rule: a final line that is not `\n`-terminated is *not consumed* —
//! `offset` stays before it, so a later reader (after the writer flushed the rest)
//! can resume exactly there. Only `\n`-terminated lines that fail to parse count
//! as malformed.

use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

use crate::model::{parse_line, RawLine};

pub struct JsonlReader {
    inner: BufReader<File>,
    /// Byte offset just past the last consumed line.
    pub offset: u64,
    /// (start, len) of the line last returned by `next_line`, for later re-reads.
    pub last_span: (u64, u64),
    pub malformed_lines: usize,
}

impl JsonlReader {
    pub fn open(path: &Path, start_offset: u64) -> std::io::Result<Self> {
        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(start_offset))?;
        Ok(Self {
            inner: BufReader::new(file),
            offset: start_offset,
            last_span: (start_offset, 0),
            malformed_lines: 0,
        })
    }

    /// Next complete parsed line; skips malformed (but complete) lines.
    /// Returns None at EOF or when only a partial line remains.
    pub fn next_line(&mut self) -> Option<RawLine> {
        loop {
            let mut buf = Vec::new();
            let n = self.inner.read_until(b'\n', &mut buf).ok()?;
            if n == 0 {
                return None; // clean EOF
            }
            if !buf.ends_with(b"\n") {
                // Partial line at EOF: leave offset before it, retry after next append.
                return None;
            }
            let start = self.offset;
            self.offset += n as u64;
            let line = String::from_utf8_lossy(&buf);
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match parse_line(trimmed) {
                Ok(raw) => {
                    self.last_span = (start, n as u64);
                    return Some(raw);
                }
                Err(_) => {
                    self.malformed_lines += 1;
                    continue;
                }
            }
        }
    }
}

/// Parsed lines paired with their byte spans (start, len).
pub type SpannedLines = Vec<(RawLine, (u64, u64))>;

/// Parse a whole file from `start_offset`. Returns each line with its byte span
/// (start, len), the offset after the last complete line, and how many complete
/// lines were malformed.
pub fn read_all(path: &Path, start_offset: u64) -> std::io::Result<(SpannedLines, u64, usize)> {
    let mut reader = JsonlReader::open(path, start_offset)?;
    let mut lines = Vec::new();
    while let Some(line) = reader.next_line() {
        lines.push((line, reader.last_span));
    }
    Ok((lines, reader.offset, reader.malformed_lines))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_tmp(content: &[u8]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content).unwrap();
        f
    }

    #[test]
    fn parses_complete_lines_and_tracks_offset() {
        let f = write_tmp(b"{\"type\":\"ai-title\",\"aiTitle\":\"T\"}\n{\"uuid\":\"u1\",\"type\":\"user\"}\n");
        let (lines, offset, bad) = read_all(f.path(), 0).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(offset, f.path().metadata().unwrap().len());
        assert_eq!(bad, 0);
        assert!(matches!(&lines[0].0, RawLine::Bookkeeping { kind, .. } if kind == "ai-title"));
        assert!(matches!(&lines[1].0, RawLine::Conversation(e) if e.uuid == "u1"));
        // Spans are contiguous and cover the file.
        assert_eq!(lines[0].1 .0, 0);
        assert_eq!(lines[1].1 .0, lines[0].1 .1);
        assert_eq!(lines[1].1 .0 + lines[1].1 .1, offset);
    }

    #[test]
    fn partial_last_line_is_not_consumed() {
        let complete = b"{\"uuid\":\"u1\",\"type\":\"user\"}\n";
        let mut content = complete.to_vec();
        content.extend_from_slice(b"{\"uuid\":\"u2\",\"ty"); // torn write
        let f = write_tmp(&content);
        let (lines, offset, _) = read_all(f.path(), 0).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(offset, complete.len() as u64);

        // Writer finishes the line: resuming at `offset` yields exactly the new entry.
        let mut file = std::fs::OpenOptions::new().append(true).open(f.path()).unwrap();
        file.write_all(b"pe\":\"user\"}\n").unwrap();
        let (lines, offset2, _) = read_all(f.path(), offset).unwrap();
        assert_eq!(lines.len(), 1);
        assert!(matches!(&lines[0].0, RawLine::Conversation(e) if e.uuid == "u2"));
        assert_eq!(lines[0].1 .0, offset); // span starts where the resume began
        assert_eq!(offset2, f.path().metadata().unwrap().len());
    }

    #[test]
    fn malformed_complete_line_is_skipped_and_counted() {
        let f = write_tmp(b"not json\n{\"uuid\":\"u1\",\"type\":\"user\"}\n");
        let (lines, _, bad) = read_all(f.path(), 0).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(bad, 1);
    }

    #[test]
    fn unknown_shapes_become_bookkeeping() {
        let f = write_tmp(b"{\"foo\":1}\n{\"type\":\"future-thing\",\"x\":2}\n");
        let (lines, _, bad) = read_all(f.path(), 0).unwrap();
        assert_eq!(bad, 0);
        assert!(matches!(&lines[0].0, RawLine::Bookkeeping { kind, .. } if kind == "unknown"));
        assert!(matches!(&lines[1].0, RawLine::Bookkeeping { kind, .. } if kind == "future-thing"));
    }
}
