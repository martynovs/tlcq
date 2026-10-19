//! Binary entry point: parse args, run, map the result to an exit code.
//!
//! `main` stays a thin shim — the logic lives in [`run`] and the library, the
//! CLI shape in [`cli`]. It is covered end-to-end by `tests/cli.rs`, which
//! spawns this binary.

use std::process::ExitCode;

use clap::Parser;

mod cli;
mod config_file;
mod laptop;
mod run;
mod status_cmd;
mod submit_cmd;

fn main() -> ExitCode {
    // The config file fills in unset `TLCQ_*` variables, which the command
    // line then reads (D13).
    if let Err(err) = config_file::load() {
        eprintln!("error: {err:#}");
        return ExitCode::from(2);
    }
    let cli = cli::Cli::parse();
    match run::run(&cli) {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            // `{err:#}` prints anyhow's full context chain.
            eprintln!("error: {err:#}");
            ExitCode::from(run::exit_code(&err))
        }
    }
}
