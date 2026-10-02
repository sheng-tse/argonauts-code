//! Argonauts' own copy of everything a vendor printed and of every frame sent to a driven agent:
//! `raw.stdout.jsonl`, `raw.stderr.txt`, and `raw.stdin.jsonl` in the session folder.
//!
//! A process spawned with [`RawLog::output_copies`] has its stdout and stderr copied byte for
//! byte as they are read, before lines are split, cut at the length cap, or decoded, so long
//! lines, carriage returns, and invalid UTF-8 all survive. The one byte added is a newline where
//! a stream stops mid-line, so a resumed process's output starts on a line of its own. Frames and
//! lines written through [`RawLog::frame`] and [`RawLog::stderr_line`] are the decoded text.

use std::fs::{File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};

use argonauts_core::create_private_dir_all;
use argonauts_core::driver::RawFrame;
use argonauts_core::layout::session_files::{RAW_STDERR, RAW_STDIN, RAW_STDOUT};
use tokio::io::{AsyncRead, ReadBuf};

/// Everything is written straight through, unbuffered, so a supervisor that crashes loses none.
#[derive(Debug)]
pub struct RawLog {
    session_dir: PathBuf,
    /// Shared with the readers of the running process, which copy into it.
    stdout: Arc<File>,
    stderr: Arc<File>,
    /// Opened with the first outgoing frame, so sessions that aren't driven get no empty file.
    stdin: Option<File>,
    /// One line and its newline, so each goes out in a single write.
    scratch: Vec<u8>,
}

impl RawLog {
    /// Creates or appends to the files in `session_dir`, private to you.
    pub fn create(session_dir: &Path) -> io::Result<Self> {
        create_private_dir_all(session_dir)?;
        Ok(Self {
            session_dir: session_dir.to_path_buf(),
            stdout: Arc::new(open_private(&session_dir.join(RAW_STDOUT))?),
            stderr: Arc::new(open_private(&session_dir.join(RAW_STDERR))?),
            stdin: None,
            scratch: Vec::new(),
        })
    }

    /// For one process's readers, which copy its output here exactly as they read it.
    pub fn output_copies(&self) -> OutputCopies {
        OutputCopies {
            stdout: RawCopy::new(Arc::clone(&self.stdout), self.stdout_path()),
            stderr: RawCopy::new(Arc::clone(&self.stderr), self.stderr_path()),
        }
    }

    /// For stderr read without [`RawLog::output_copies`]. The line is written as decoded.
    pub fn stderr_line(&mut self, line: &str) -> io::Result<()> {
        append_line(&self.stderr, &mut self.scratch, line)
    }

    /// Frames sent to the agent go to `raw.stdin.jsonl`, and frames from it to
    /// `raw.stdout.jsonl`.
    pub fn frame(&mut self, frame: &RawFrame) -> io::Result<()> {
        if !frame.outgoing {
            return append_line(&self.stdout, &mut self.scratch, &frame.line);
        }
        let stdin = match &mut self.stdin {
            Some(file) => file,
            empty => empty.insert(open_private(&self.session_dir.join(RAW_STDIN))?),
        };
        append_line(stdin, &mut self.scratch, &frame.line)
    }

    /// For `SessionEnd.raw_log`.
    pub fn stdout_path(&self) -> PathBuf {
        self.session_dir.join(RAW_STDOUT)
    }

    pub fn stderr_path(&self) -> PathBuf {
        self.session_dir.join(RAW_STDERR)
    }

    pub fn stdin_path(&self) -> PathBuf {
        self.session_dir.join(RAW_STDIN)
    }
}

/// Where one process's stdout and stderr are copied.
#[derive(Debug)]
pub struct OutputCopies {
    pub stdout: RawCopy,
    pub stderr: RawCopy,
}

/// One stream's raw log file, fed by [`CopyingReader`]. It stops at the first failed write, so
/// the file never has a gap in the middle.
#[derive(Debug)]
pub struct RawCopy {
    file: Arc<File>,
    path: PathBuf,
    mid_line: bool,
    failed: bool,
}

impl RawCopy {
    fn new(file: Arc<File>, path: PathBuf) -> Self {
        Self {
            file,
            path,
            mid_line: false,
            failed: false,
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        let Some(&last) = bytes.last() else {
            return;
        };
        if self.failed {
            return;
        }
        if let Err(err) = (&*self.file).write_all(bytes) {
            self.failed = true;
            tracing::warn!("stopped writing the raw log {}: {err}", self.path.display());
            return;
        }
        self.mid_line = last != b'\n';
    }
}

impl Drop for RawCopy {
    fn drop(&mut self) {
        if self.mid_line {
            self.write(b"\n");
        }
    }
}

/// Reads from `inner` and copies every byte to the raw log before anyone else sees it.
#[derive(Debug)]
pub struct CopyingReader<R> {
    inner: R,
    copy: Option<RawCopy>,
}

impl<R> CopyingReader<R> {
    /// With no copy, it reads straight through.
    pub fn new(inner: R, copy: Option<RawCopy>) -> Self {
        Self { inner, copy }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for CopyingReader<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let start = buf.filled().len();
        ready!(Pin::new(&mut this.inner).poll_read(cx, buf))?;
        if let Some(copy) = &mut this.copy {
            copy.write(&buf.filled()[start..]);
        }
        Poll::Ready(Ok(()))
    }
}

fn open_private(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(path)?;
    // The mode applies only to a new file, and one left from an earlier run may be wider.
    file.set_permissions(Permissions::from_mode(0o600))?;
    Ok(file)
}

fn append_line(mut file: &File, scratch: &mut Vec<u8>, line: &str) -> io::Result<()> {
    scratch.clear();
    scratch.extend_from_slice(line.as_bytes());
    scratch.push(b'\n');
    file.write_all(scratch)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(dir: &Path, name: &str) -> String {
        std::fs::read_to_string(dir.join(name)).unwrap()
    }

    fn mode(dir: &Path, name: &str) -> u32 {
        std::fs::metadata(dir.join(name))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    }

    async fn copy_through(copy: RawCopy, input: &[u8]) {
        let mut reader = CopyingReader::new(input, Some(copy));
        let mut sink = Vec::new();
        tokio::io::copy(&mut reader, &mut sink).await.unwrap();
        assert_eq!(sink, input);
    }

    #[tokio::test]
    async fn output_is_copied_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("s-001");
        let log = RawLog::create(&session).unwrap();
        let stdout: &[u8] = b"{\"type\":\"system\"}\r\n\xff\xfe not utf-8\n\nlast";
        let stderr: &[u8] = b"warning\r\n";
        let OutputCopies {
            stdout: out,
            stderr: err,
        } = log.output_copies();
        copy_through(out, stdout).await;
        copy_through(err, stderr).await;

        // A stream that stopped mid-line gets its newline, so the next process's output starts on
        // a line of its own.
        let next = log.output_copies();
        copy_through(next.stdout, b"resumed\n").await;
        drop(next.stderr);

        let mut expected = stdout.to_vec();
        expected.extend_from_slice(b"\nresumed\n");
        assert_eq!(std::fs::read(session.join(RAW_STDOUT)).unwrap(), expected);
        assert_eq!(std::fs::read(session.join(RAW_STDERR)).unwrap(), stderr);
    }

    #[test]
    fn raw_log_writes_every_line_and_frame() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("sessions").join("s-001");
        let mut log = RawLog::create(&session).unwrap();
        log.stderr_line("warning: something").unwrap();
        log.stderr_line("").unwrap();
        assert!(!log.stdin_path().exists());

        log.frame(&RawFrame {
            outgoing: true,
            line: r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#.into(),
        })
        .unwrap();
        log.frame(&RawFrame {
            outgoing: false,
            line: r#"{"jsonrpc":"2.0","id":1,"result":{}}"#.into(),
        })
        .unwrap();
        assert_eq!(log.stdout_path(), session.join(RAW_STDOUT));

        assert_eq!(
            read(&session, RAW_STDOUT),
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n"
        );
        assert_eq!(read(&session, RAW_STDERR), "warning: something\n\n");
        assert_eq!(
            read(&session, RAW_STDIN),
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}\n"
        );
        for name in [RAW_STDOUT, RAW_STDERR, RAW_STDIN] {
            assert_eq!(mode(&session, name), 0o600, "{name}");
        }
        let dir_mode = std::fs::metadata(&session).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);

        // A resumed session appends to what's there, and a wide file is made private again.
        std::fs::set_permissions(session.join(RAW_STDERR), Permissions::from_mode(0o644)).unwrap();
        drop(log);
        let mut log = RawLog::create(&session).unwrap();
        log.stderr_line("second run").unwrap();
        assert_eq!(
            read(&session, RAW_STDERR),
            "warning: something\n\nsecond run\n"
        );
        assert_eq!(mode(&session, RAW_STDERR), 0o600);
    }
}
