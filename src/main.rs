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
