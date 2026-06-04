use std::env;
use std::io::{self, Write};
use std::process::ExitCode;

use cds::cli::{Invocation, parse_invocation};
use cds::{emit_cd_script, shell_init};
use clap::error::ErrorKind;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => err.exit(),
    }
}

fn run() -> Result<(), clap::Error> {
    match parse_invocation(env::args_os().skip(1))? {
        Invocation::EmitCd { args } => write_stdout(&emit_cd_script(&args)),
        Invocation::ShellInit { shell } => write_stdout(shell_init(shell).as_bytes()),
    }
    .map_err(write_error)
}

fn write_stdout(bytes: &[u8]) -> io::Result<()> {
    io::stdout().write_all(bytes)
}

fn write_error(err: io::Error) -> clap::Error {
    clap::Error::raw(ErrorKind::Io, format!("cds: failed to write stdout: {err}"))
}
