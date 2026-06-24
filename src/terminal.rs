use std::{cell::RefCell, io};

use anyhow::{Result, anyhow};
use crossterm::{
    cursor::{Hide, Show},
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{
        Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode,
    },
};
use ratatui::{Terminal, backend::CrosstermBackend};

pub(crate) struct TerminalGuard {
    terminal: RefCell<Option<Terminal<CrosstermBackend<io::Stdout>>>>,
}

impl TerminalGuard {
    pub(crate) fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(
            stdout,
            EnterAlternateScreen,
            EnableMouseCapture,
            Hide,
            Clear(ClearType::All)
        )?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(Self {
            terminal: RefCell::new(Some(terminal)),
        })
    }

    pub(crate) fn terminal(
        &self,
    ) -> Result<impl std::ops::DerefMut<Target = Terminal<CrosstermBackend<io::Stdout>>> + '_> {
        std::cell::RefMut::filter_map(self.terminal.borrow_mut(), Option::as_mut)
            .map_err(|_| anyhow!("terminal already left"))
    }

    pub(crate) fn leave(&self) -> Result<()> {
        if let Some(mut terminal) = self.terminal.borrow_mut().take() {
            execute!(
                terminal.backend_mut(),
                Show,
                DisableMouseCapture,
                LeaveAlternateScreen
            )?;
            disable_raw_mode()?;
        }
        Ok(())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if let Some(terminal) = self.terminal.get_mut() {
            let _ = execute!(
                terminal.backend_mut(),
                Show,
                DisableMouseCapture,
                LeaveAlternateScreen
            );
        }
        let _ = disable_raw_mode();
    }
}
