use std::{cell::RefCell, io};

use anyhow::{Result, anyhow};
use crossterm::{
    cursor::{Hide, Show},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};

pub(crate) struct TerminalGuard {
    stdout: RefCell<Option<io::Stdout>>,
}

impl TerminalGuard {
    pub(crate) fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, Hide)?;
        Ok(Self {
            stdout: RefCell::new(Some(stdout)),
        })
    }

    pub(crate) fn stdout(&self) -> Result<impl std::ops::DerefMut<Target = io::Stdout> + '_> {
        std::cell::RefMut::filter_map(self.stdout.borrow_mut(), Option::as_mut)
            .map_err(|_| anyhow!("terminal already left"))
    }

    pub(crate) fn leave(self) -> Result<()> {
        if let Some(mut stdout) = self.stdout.borrow_mut().take() {
            execute!(stdout, Show, LeaveAlternateScreen)?;
        }
        disable_raw_mode()?;
        Ok(())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if let Some(stdout) = self.stdout.get_mut() {
            let _ = execute!(stdout, Show, LeaveAlternateScreen);
        }
        let _ = disable_raw_mode();
    }
}
