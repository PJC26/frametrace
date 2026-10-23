//! Runs a child process and streams its output lines to a [`Reporter`]
//! without ever waiting for pipe EOF.
//!
//! Build tools spawn daemons (pacman-key starts `gpg-agent`) that inherit
//! the child's stdout/stderr handles and outlive it, so EOF may never come.
//! Completion is therefore decided by the child's exit status; output that
//! is still in flight is drained for a short grace period, and the reader
//! threads are left detached. Lines are decoded lossily because console
//! tools print in the OEM code page (CP949), not UTF-8.

use super::Reporter;
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(250);
const DRAIN_GRACE: Duration = Duration::from_secs(2);

pub(super) struct StreamOutcome {
    pub(super) status: ExitStatus,
    /// Last non-empty line seen on either stream (error detail on failure).
    pub(super) last_line: String,
}

pub(super) fn run_streaming(command: &mut Command, report: Reporter<'_>) -> Result<StreamOutcome, String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("프로세스 실행 실패: {err}"))?;
    let (tx, rx) = mpsc::channel();
    if let Some(out) = child.stdout.take() {
        spawn_line_reader(out, tx.clone());
    }
    if let Some(err) = child.stderr.take() {
        spawn_line_reader(err, tx);
    }
    let mut last_line = String::new();
    let status = wait_reporting(&mut child, &rx, report, &mut last_line)?;
    let deadline = Instant::now() + DRAIN_GRACE;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(line) => accept(line, report, &mut last_line),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(StreamOutcome { status, last_line })
}

fn wait_reporting(
    child: &mut Child,
    rx: &Receiver<String>,
    report: Reporter<'_>,
    last_line: &mut String,
) -> Result<ExitStatus, String> {
    loop {
        if let Some(status) = child.try_wait().map_err(|err| format!("프로세스 대기 실패: {err}"))? {
            return Ok(status);
        }
        match rx.recv_timeout(POLL) {
            Ok(line) => accept(line, report, last_line),
            // Disconnected = both pipes closed; keep polling the exit status.
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {}
        }
    }
}

fn accept(line: String, report: Reporter<'_>, last_line: &mut String) {
    report(line.clone());
    *last_line = line;
}

fn spawn_line_reader(pipe: impl Read + Send + 'static, tx: Sender<String>) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(pipe);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => return,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf).trim().to_string();
                    if !line.is_empty() && tx.send(line).is_err() {
                        return;
                    }
                }
            }
        }
    });
}

#[cfg(test)]
#[cfg(windows)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn cmd(script: &str) -> Command {
        let mut command = Command::new("cmd");
        command.args(["/d", "/c", script]);
        command
    }

    #[test]
    fn returns_when_a_grandchild_keeps_the_pipes_open() {
        // Given: a child that leaves a background process holding stderr
        let mut command = cmd("start /b ping -n 30 127.0.0.1 >nul & echo built");
        let seen = Mutex::new(Vec::new());
        let started = Instant::now();
        // When: it is streamed
        let outcome = run_streaming(&mut command, &|line| seen.lock().unwrap().push(line)).unwrap();
        // Then: completion follows the child's exit, not pipe EOF
        assert!(started.elapsed() < Duration::from_secs(15), "hung for {:?}", started.elapsed());
        assert!(outcome.status.success());
        assert!(seen.lock().unwrap().iter().any(|l| l == "built"));
    }

    #[test]
    fn keeps_reading_past_non_utf8_lines() {
        // Given: a CP949 line ("가" = B0 A1, invalid UTF-8) then a normal one
        let bytes = b"\xB0\xA1\r\nafter\r\n".to_vec();
        let (tx, rx) = mpsc::channel();
        // When
        spawn_line_reader(std::io::Cursor::new(bytes), tx);
        let lines: Vec<String> = rx.iter().collect();
        // Then: decoding is lossy and the following line still arrives
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[1], "after");
    }

    #[test]
    fn reports_failure_status_and_last_line() {
        let mut command = cmd("echo boom 1>&2 & exit /b 3");
        let outcome = run_streaming(&mut command, &|_| {}).unwrap();
        assert_eq!(outcome.status.code(), Some(3));
        assert_eq!(outcome.last_line, "boom");
    }
}
