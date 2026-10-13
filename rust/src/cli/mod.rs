//! The `typedframes` command line.

mod args;
mod cache;
mod check;
mod collect;
mod config;
mod coverage;
mod pathutil;
mod pyfmt;
mod report;
mod style;
mod suggest;

use args::Command;
use clap::CommandFactory;
use std::io::{self, BufWriter, IsTerminal, LineWriter, Write};
use std::process::ExitCode;

fn run_command(command: Command, out: &mut impl Write) -> io::Result<i32> {
    match command {
        Command::Check(args) => check::run_check(&args, out),
        Command::Cache(args) => {
            cache::run_cache(args.action, &args.path, args.cache_dir.as_deref(), out)?;
            Ok(0)
        }
        Command::Suggest(args) => suggest::run_suggest(&args, out, &mut io::stdin().lock()),
        Command::Report(args) => report::run_report(&args, out),
    }
}

/// Parses the process arguments, runs the command and returns the exit status.
pub fn main() -> ExitCode {
    let cli = args::parse();
    // On a terminal output appears line by line, in step with stderr; to a pipe or file it is
    // written in large blocks.
    let stdout = io::stdout();
    let mut out: Box<dyn Write> = if stdout.is_terminal() {
        Box::new(LineWriter::new(stdout.lock()))
    } else {
        Box::new(BufWriter::new(stdout.lock()))
    };
    let status = match cli.command {
        Some(command) => run_command(command, &mut out),
        None => {
            let _ = args::Cli::command().print_help();
            Ok(2)
        }
    };
    // A closed pipe (`typedframes check | head`) is not an error worth a message.
    let flushed = out.flush();
    match (status, flushed) {
        (Ok(code), Ok(())) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        (Err(error), _) | (_, Err(error)) if error.kind() == io::ErrorKind::BrokenPipe => {
            ExitCode::SUCCESS
        }
        (Err(error), _) | (_, Err(error)) => {
            eprintln!("Error: {error}");
            ExitCode::from(1)
        }
    }
}
