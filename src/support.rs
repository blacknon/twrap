use anyhow::Result;
use crossterm::{cursor::Show, execute, terminal::disable_raw_mode};
use std::{
    ffi::OsString,
    io::{self, IsTerminal},
};

pub(crate) fn restore_terminal_state() {
    if io::stdout().is_terminal() {
        let _ = execute!(io::stdout(), Show, crossterm::style::ResetColor);
        let _ = disable_raw_mode();
    }
}

pub(crate) fn exit_with_status(code: i32) -> ! {
    restore_terminal_state();
    std::process::exit(code);
}

pub(crate) fn spawn_direct(command: &[OsString]) -> Result<()> {
    let mut cmd = std::process::Command::new(&command[0]);
    cmd.args(&command[1..]);
    if let Ok(cwd) = std::env::current_dir() {
        cmd.current_dir(cwd);
    }
    let status = cmd.status()?;
    exit_with_status(status.code().unwrap_or(1));
}
