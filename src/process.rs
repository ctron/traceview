use std::{
    io::{self, BufReader},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, Sender},
    },
    thread,
};

use anyhow::{Context, Result, anyhow};

use crate::model::{AppEvent, Stream};

pub(crate) const DEFAULT_MAX_LINE_BYTES: usize = 64 * 1024;

pub(crate) struct RunningCommand {
    pub(crate) events: Receiver<AppEvent>,
    child: Arc<Mutex<Child>>,
}

impl RunningCommand {
    pub(crate) fn terminate(&self) {
        let Ok(mut child) = self.child.lock() else {
            return;
        };

        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }

        let _ = child.kill();
    }
}

pub(crate) fn spawn_command(command: &[String], max_line_bytes: usize) -> Result<RunningCommand> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| anyhow!("missing command"))?;

    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn `{program}`"))?;

    let stdout = child.stdout.take().context("failed to capture stdout")?;
    let stderr = child.stderr.take().context("failed to capture stderr")?;

    let (tx, rx) = mpsc::channel();
    spawn_reader(Stream::Stdout, stdout, tx.clone(), max_line_bytes);
    spawn_reader(Stream::Stderr, stderr, tx.clone(), max_line_bytes);

    let child = Arc::new(Mutex::new(child));
    let waiter_child = Arc::clone(&child);
    thread::spawn(move || {
        let event = wait_for_child(waiter_child);
        let _ = tx.send(event);
    });

    Ok(RunningCommand { events: rx, child })
}

fn wait_for_child(child: Arc<Mutex<Child>>) -> AppEvent {
    loop {
        let status = match child.lock() {
            Ok(mut child) => child.try_wait(),
            Err(err) => {
                return AppEvent::ReaderFailed(Stream::Stderr, format!("wait lock failed: {err}"));
            }
        };

        match status {
            Ok(Some(status)) => return AppEvent::ProcessExited(status),
            Ok(None) => thread::sleep(std::time::Duration::from_millis(50)),
            Err(err) => {
                return AppEvent::ReaderFailed(Stream::Stderr, format!("wait failed: {err}"));
            }
        }
    }
}

fn spawn_reader<R>(stream: Stream, reader: R, tx: Sender<AppEvent>, max_line_bytes: usize)
where
    R: io::Read + Send + 'static,
{
    thread::spawn(move || {
        let reader = BufReader::new(reader);
        if let Err(err) = StreamReader::new(stream, reader, max_line_bytes, &tx).read_lines() {
            let _ = tx.send(AppEvent::ReaderFailed(stream, err.to_string()));
        }
    });
}

struct StreamReader<'a, R> {
    stream: Stream,
    reader: BufReader<R>,
    max_line_bytes: usize,
    tx: &'a Sender<AppEvent>,
    line: Vec<u8>,
    truncated: bool,
}

impl<'a, R> StreamReader<'a, R>
where
    R: io::Read,
{
    fn new(
        stream: Stream,
        reader: BufReader<R>,
        max_line_bytes: usize,
        tx: &'a Sender<AppEvent>,
    ) -> Self {
        Self {
            stream,
            reader,
            max_line_bytes,
            tx,
            line: Vec::new(),
            truncated: false,
        }
    }

    fn read_lines(mut self) -> io::Result<()> {
        loop {
            let ends_line = {
                let buffer = io::BufRead::fill_buf(&mut self.reader)?;
                if buffer.is_empty() {
                    if !self.line.is_empty() || self.truncated {
                        let _ = self.send_line();
                    }
                    return Ok(());
                }

                if let Some(newline_idx) = buffer.iter().position(|byte| *byte == b'\n') {
                    append_line_bytes(
                        &mut self.line,
                        &buffer[..newline_idx],
                        self.max_line_bytes,
                        &mut self.truncated,
                    );
                    io::BufRead::consume(&mut self.reader, newline_idx + 1);
                    true
                } else {
                    let consumed = buffer.len();
                    append_line_bytes(
                        &mut self.line,
                        buffer,
                        self.max_line_bytes,
                        &mut self.truncated,
                    );
                    io::BufRead::consume(&mut self.reader, consumed);
                    false
                }
            };

            if ends_line && !self.send_line() {
                return Ok(());
            }
        }
    }

    fn send_line(&mut self) -> bool {
        let mut line = String::from_utf8_lossy(&self.line).into_owned();
        if self.truncated {
            line.push_str(" ... [truncated]");
        }
        self.line.clear();
        self.truncated = false;
        self.tx.send(AppEvent::Line(self.stream, line)).is_ok()
    }
}

fn append_line_bytes(
    line: &mut Vec<u8>,
    bytes: &[u8],
    max_line_bytes: usize,
    truncated: &mut bool,
) {
    let remaining = max_line_bytes.saturating_sub(line.len());
    if remaining >= bytes.len() {
        line.extend_from_slice(bytes);
    } else {
        line.extend_from_slice(&bytes[..remaining]);
        *truncated = true;
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn read_test_lines(input: &[u8], max_line_bytes: usize) -> Vec<String> {
        let (tx, rx) = mpsc::channel();
        StreamReader::new(
            Stream::Stdout,
            BufReader::new(Cursor::new(input.to_vec())),
            max_line_bytes,
            &tx,
        )
        .read_lines()
        .expect("read lines");
        drop(tx);

        rx.into_iter()
            .filter_map(|event| match event {
                AppEvent::Line(_, line) => Some(line),
                AppEvent::ReaderFailed(_, _) | AppEvent::ProcessExited(_) => None,
            })
            .collect()
    }

    #[test]
    fn bounded_reader_keeps_short_lines() {
        assert_eq!(
            read_test_lines(b"alpha\nbeta\n", 10),
            vec!["alpha".to_string(), "beta".to_string()]
        );
    }

    #[test]
    fn bounded_reader_truncates_long_lines_and_recovers_at_newline() {
        assert_eq!(
            read_test_lines(b"abcdefghijkl\nnext\n", 5),
            vec!["abcde ... [truncated]".to_string(), "next".to_string()]
        );
    }

    #[test]
    fn bounded_reader_truncates_final_line_without_newline() {
        assert_eq!(
            read_test_lines(b"abcdefghijkl", 5),
            vec!["abcde ... [truncated]".to_string()]
        );
    }

    #[test]
    fn bounded_reader_uses_lossy_utf8_for_partial_codepoints() {
        assert_eq!(
            read_test_lines("aé\n".as_bytes(), 2),
            vec!["a\u{fffd} ... [truncated]".to_string()]
        );
    }
}
