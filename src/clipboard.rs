use std::io::{self, Write};

use anyhow::Result;
use base64::{Engine as _, engine::general_purpose::STANDARD};

pub(crate) fn copy_to_clipboard(text: &str) -> Result<()> {
    let encoded = STANDARD.encode(text);
    let mut stdout = io::stdout();
    write!(stdout, "\x1b]52;c;{encoded}\x07")?;
    stdout.flush()?;
    Ok(())
}
