mod config;
mod highlight;
mod model;
mod support;
mod template;
mod transform;
mod tui;

use anyhow::Result;
use clap::Parser;
use model::Cli;

fn main() -> Result<()> {
    let cli = Cli::parse();
    let runtime = config::build_runtime(cli)?;
    tui::run(runtime)
}
