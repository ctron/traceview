#![deny(
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::unwrap_used
)]

use anyhow::Result;
use clap::Parser;

mod app;
mod cli;
mod clipboard;
mod model;
mod parser;
mod process;
mod terminal;
mod ui;

use cli::Cli;

fn main() -> Result<()> {
    app::run(Cli::parse())
}
