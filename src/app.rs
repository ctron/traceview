use std::{
    collections::VecDeque,
    num::NonZeroUsize,
    time::{Duration, Instant},
};

use anyhow::Result;
use crossterm::{
    event::{self, Event},
    terminal,
};

use crate::{
    cli::{Cli, LogFormat},
    clipboard::copy_to_clipboard,
    model::{AppEvent, Level, LogEntry},
    parser::parse_log_line,
    process::{InputSource, RunningInput, spawn_input},
    terminal::TerminalGuard,
    ui::{KeyAction, ViewState, content_rows, draw, handle_key, handle_mouse, selected_line_text},
};

const MAX_EVENTS_PER_TICK: usize = 1024;

pub(crate) fn run(cli: Cli) -> Result<()> {
    let source = if let Some(file) = &cli.file {
        InputSource::File(file)
    } else {
        InputSource::Command(&cli.command)
    };
    let input = spawn_input(source, cli.max_line_bytes)?;

    let terminal = TerminalGuard::enter()?;
    let result = event_loop(&terminal, &input, cli.format, cli.max_lines);
    terminal.leave()?;

    result
}

fn event_loop(
    terminal: &TerminalGuard,
    input: &RunningInput,
    format: LogFormat,
    max_lines: Option<NonZeroUsize>,
) -> Result<()> {
    let mut entries = VecDeque::new();
    let mut state = ViewState::new();
    let mut exit_status = None;
    let mut input_finished = false;
    let mut last_draw = Instant::now() - Duration::from_secs(1);
    let mut dirty = true;

    loop {
        let page_size = content_rows(terminal::size()?.1, &state);

        for _ in 0..MAX_EVENTS_PER_TICK {
            if !event::poll(Duration::ZERO)? {
                break;
            }
            let outcome = handle_terminal_event(
                event::read()?,
                terminal,
                input,
                &entries,
                &mut state,
                exit_status.is_some() || input_finished,
                page_size,
            )?;
            if outcome.exit {
                return Ok(());
            }
            dirty |= outcome.redraw;
        }

        for _ in 0..MAX_EVENTS_PER_TICK {
            let Ok(app_event) = input.events.try_recv() else {
                break;
            };
            match app_event {
                AppEvent::Line(stream, line) => {
                    let was_following_latest = state
                        .selected
                        .is_none_or(|selected| selected + 1 == entries.len());

                    if max_lines.is_some_and(|max_lines| entries.len() == max_lines.get()) {
                        entries.pop_front();
                        state.remove_first_line();
                    }
                    entries.push_back(parse_log_line(format, stream, line));
                    if was_following_latest {
                        state.follow_latest(&entries, page_size);
                    }
                    dirty = true;
                }
                AppEvent::ProcessExited(status) => {
                    exit_status = Some(status);
                    dirty = true;
                }
                AppEvent::InputFinished => {
                    input_finished = true;
                    dirty = true;
                }
                AppEvent::ReaderFailed(stream, err) => {
                    let message = format!("{stream:?} reader failed: {err}");
                    entries.push_back(LogEntry {
                        raw: message.clone(),
                        timestamp: None,
                        level: Level::Error,
                        parsed: false,
                        thread: None,
                        target: Some("traceviewer".to_string()),
                        spans: Vec::new(),
                        values: Vec::new(),
                        message,
                        message_parts: Vec::new(),
                        stream,
                    });
                    dirty = true;
                }
            }
        }

        if dirty && last_draw.elapsed() >= Duration::from_millis(16) {
            let mut tui = terminal.terminal()?;
            tui.draw(|frame| draw(frame, &entries, &state, exit_status, input_finished))?;
            last_draw = Instant::now();
            dirty = false;
        }

        if event::poll(Duration::from_millis(50))? {
            let outcome = handle_terminal_event(
                event::read()?,
                terminal,
                input,
                &entries,
                &mut state,
                exit_status.is_some() || input_finished,
                page_size,
            )?;
            if outcome.exit {
                return Ok(());
            }
            dirty |= outcome.redraw;
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct TerminalEventOutcome {
    exit: bool,
    redraw: bool,
}

fn handle_terminal_event(
    event: Event,
    terminal: &TerminalGuard,
    input: &RunningInput,
    entries: &VecDeque<LogEntry>,
    state: &mut ViewState,
    input_finished: bool,
    page_size: usize,
) -> Result<TerminalEventOutcome> {
    let redraw = terminal_event_requests_redraw(&event);
    match event {
        Event::Key(key) => Ok(TerminalEventOutcome {
            exit: handle_terminal_key(
                key,
                terminal,
                input,
                entries,
                state,
                input_finished,
                page_size,
            )?,
            redraw,
        }),
        Event::Mouse(mouse) => {
            let action = handle_mouse(mouse, entries, state, input_finished, page_size);
            debug_assert_eq!(action, KeyAction::Continue);
            Ok(TerminalEventOutcome {
                exit: false,
                redraw,
            })
        }
        Event::Resize(_, _) => Ok(TerminalEventOutcome {
            exit: false,
            redraw,
        }),
        _ => Ok(TerminalEventOutcome::default()),
    }
}

fn terminal_event_requests_redraw(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(_)
            | Event::Resize(_, _)
            | Event::Mouse(crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::ScrollUp
                    | crossterm::event::MouseEventKind::ScrollDown,
                ..
            })
    )
}

fn handle_terminal_key(
    key: crossterm::event::KeyEvent,
    terminal: &TerminalGuard,
    input: &RunningInput,
    entries: &VecDeque<LogEntry>,
    state: &mut ViewState,
    input_finished: bool,
    page_size: usize,
) -> Result<bool> {
    match handle_key(key, entries, state, input_finished, page_size) {
        KeyAction::Continue => Ok(false),
        KeyAction::CopySelected => {
            if let Some(line) = selected_line_text(entries, state) {
                copy_to_clipboard(&line)?;
            }
            Ok(false)
        }
        KeyAction::Quit => {
            terminal.leave()?;
            Ok(true)
        }
        KeyAction::KillAndExit => {
            let _ = terminal.leave();
            input.terminate();
            std::process::exit(130);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};

    fn mouse_event(kind: MouseEventKind) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        })
    }

    #[test]
    fn mouse_motion_does_not_request_redraw() {
        assert!(!terminal_event_requests_redraw(&mouse_event(
            MouseEventKind::Moved
        )));
    }

    #[test]
    fn mouse_wheel_requests_redraw() {
        assert!(terminal_event_requests_redraw(&mouse_event(
            MouseEventKind::ScrollUp
        )));
        assert!(terminal_event_requests_redraw(&mouse_event(
            MouseEventKind::ScrollDown
        )));
    }
}
