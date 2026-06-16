use std::{
    io::{self, BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, Sender},
    },
    thread,
};

use anyhow::{Context, Result, anyhow};

use crate::model::{AppEvent, Stream};

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

pub(crate) fn spawn_command(command: &[String]) -> Result<RunningCommand> {
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
    spawn_reader(Stream::Stdout, stdout, tx.clone());
    spawn_reader(Stream::Stderr, stderr, tx.clone());

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

fn spawn_reader<R>(stream: Stream, reader: R, tx: Sender<AppEvent>)
where
    R: io::Read + Send + 'static,
{
    thread::spawn(move || {
        let reader = BufReader::new(reader);
        for line in reader.lines() {
            match line {
                Ok(line) => {
                    if tx.send(AppEvent::Line(stream, line)).is_err() {
                        break;
                    }
                }
                Err(err) => {
                    let _ = tx.send(AppEvent::ReaderFailed(stream, err.to_string()));
                    break;
                }
            }
        }
    });
}
