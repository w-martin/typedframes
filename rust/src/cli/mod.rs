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
use std::ffi::OsString;
use std::io::{self, BufWriter, IsTerminal, LineWriter, Write};
use std::process::ExitCode;

fn run_command(command: Command, out: &mut impl Write, err: &mut impl Write) -> io::Result<i32> {
    match command {
        Command::Check(args) => check::run_check(&args, out, err),
        Command::Cache(args) => {
            cache::run_cache(args.action, &args.path, args.cache_dir.as_deref(), out)?;
            Ok(0)
        }
        Command::Suggest(args) => suggest::run_suggest(&args, out, &mut io::stdin().lock()),
        Command::Report(args) => report::run_report(&args, out),
    }
}

/// Parses `args` (program name first). A bad command line, `--help` and `--version` come back as
/// the clap error, with the status it asks for.
fn parse(
    args: impl IntoIterator<Item = impl Into<OsString> + Clone>,
) -> Result<args::Cli, (clap::Error, u8)> {
    args::parse_from(args).map_err(|error| {
        let status = u8::try_from(error.exit_code()).unwrap_or(2);
        (error, status)
    })
}

/// Runs the parsed command line, writing results to `out` and a failure's message to `err`;
/// returns the exit status.
fn execute(cli: args::Cli, out: &mut impl Write, err: &mut impl Write) -> u8 {
    let status = match cli.command {
        Some(command) => run_command(command, out, err),
        None => {
            let _ = args::Cli::command().print_help();
            Ok(2)
        }
    };
    // A closed pipe (`typedframes check | head`) is not an error worth a message.
    let flushed = out.flush();
    match (status, flushed) {
        (Ok(code), Ok(())) => u8::try_from(code).unwrap_or(1),
        (Err(error), _) | (_, Err(error)) if error.kind() == io::ErrorKind::BrokenPipe => 0,
        (Err(error), _) | (_, Err(error)) => {
            let _ = writeln!(err, "Error: {error}");
            1
        }
    }
}

/// Runs the command line `args` (program name first) and returns the exit status.
///
/// Never ends the process, so an interpreter hosting the checker survives a usage error.
pub fn run<I, T>(args: I) -> u8
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match parse(args) {
        Ok(cli) => cli,
        Err((error, status)) => {
            let _ = error.print();
            return status;
        }
    };
    // On a terminal output appears line by line, in step with stderr; to a pipe or file it is
    // written in large blocks.
    let stdout = io::stdout();
    let mut out: Box<dyn Write> = if stdout.is_terminal() {
        Box::new(LineWriter::new(stdout.lock()))
    } else {
        Box::new(BufWriter::new(stdout.lock()))
    };
    execute(cli, &mut out, &mut io::stderr())
}

/// Runs the command line `args` and returns the exit status with what it wrote to standard output
/// and, for a usage error or a failed command, to standard error. Anything else a command prints to
/// standard error (progress, warnings) still goes to the process's own.
pub fn run_captured<I, T>(args: I) -> (u8, Vec<u8>, Vec<u8>)
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let status = match parse(args) {
        Ok(cli) => execute(cli, &mut out, &mut err),
        Err((error, status)) => {
            let target = if error.use_stderr() {
                &mut err
            } else {
                &mut out
            };
            let _ = write!(target, "{}", error.render());
            status
        }
    };
    (status, out, err)
}

/// Runs the process's own command line.
pub fn main() -> ExitCode {
    ExitCode::from(run(std::env::args_os()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_run_returns_the_output_instead_of_printing_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.py"), "x = 1\n").unwrap();
        let path = dir.path().to_str().unwrap();

        let (status, out, err) = run_captured([
            "typedframes",
            "check",
            path,
            "--no-cache",
            "--output-format",
            "json",
        ]);

        assert_eq!(status, 0);
        assert!(String::from_utf8(out).unwrap().contains("\"errors\""));
        assert!(err.is_empty());
    }

    #[test]
    fn a_bad_command_line_is_a_status_and_a_message_not_an_exit() {
        let (status, out, err) = run_captured(["typedframes", "no-such-command"]);

        assert_eq!(status, 2);
        assert!(out.is_empty());
        assert!(String::from_utf8(err).unwrap().contains("no-such-command"));
    }

    #[test]
    fn help_is_a_success_written_to_standard_output() {
        let (status, out, err) = run_captured(["typedframes", "--help"]);

        assert_eq!(status, 0);
        assert!(!out.is_empty());
        assert!(err.is_empty());
    }

    #[test]
    fn a_failed_command_leaves_its_message_in_the_captured_error() {
        let (status, out, err) = run_captured([
            "typedframes",
            "check",
            "/no/such/path/anywhere",
            "--no-cache",
        ]);

        assert_eq!(status, 2);
        assert!(out.is_empty());
        assert!(String::from_utf8(err)
            .unwrap()
            .contains("path does not exist"));
    }
}
