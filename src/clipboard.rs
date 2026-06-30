use std::io::Write;

use anyhow::Result;
use arboard::Clipboard;
use base64::{Engine as _, engine::general_purpose::STANDARD};

pub(crate) fn copy_to_clipboard(text: &str, osc52_writer: &mut impl Write) -> Result<()> {
    if copy_to_arboard(text).is_ok() {
        return Ok(());
    }

    copy_to_osc52(text, osc52_writer)
}

fn copy_to_arboard(text: &str) -> Result<()> {
    Clipboard::new()?.set_text(text.to_string())?;
    Ok(())
}

fn copy_to_osc52(text: &str, writer: &mut impl Write) -> Result<()> {
    write_osc52(text, writer)?;
    writer.flush()?;
    Ok(())
}

fn write_osc52(text: &str, writer: &mut impl Write) -> Result<()> {
    let encoded = STANDARD.encode(text);
    write!(writer, "\x1b]52;c;{encoded}\x07")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc52_encodes_text_for_clipboard() {
        let mut output = Vec::new();

        let result = write_osc52("copy me", &mut output);

        assert!(result.is_ok());
        assert_eq!(output, b"\x1b]52;c;Y29weSBtZQ==\x07");
    }
}
