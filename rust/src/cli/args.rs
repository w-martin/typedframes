//! The command line's arguments.

use super::cache::CacheAction;
use super::config::CoverageDetail;
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::ffi::OsString;
use std::path::PathBuf;

const CACHE_HELP: &str = "\
The index (what the checker learns about every file, so imports across files resolve) is
cached on disk, per file, and reused for any file whose contents have not changed. A
cached run gives exactly the results an uncached run would.

Where it lives, first match wins:
  --cache-dir PATH                      on the command line
  TYPEDFRAMES_CACHE_DIR=PATH            in the environment
  [tool.typedframes] cache_dir = \"PATH\" in pyproject.toml (relative to the project)
  the per-user cache directory          the default (see `typedframes cache path`)
Turn it off for one run with --no-cache.

Examples:
  typedframes cache path                  where is it?
  typedframes cache clear                 delete it
  typedframes cache gitignore             keep a project-local cache out of git
  typedframes check src/ --cache-dir .typedframes_cache";

#[derive(Parser, Debug)]
#[command(
    name = "typedframes",
    about = "Static analysis for DataFrame column schemas."
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    /// Check Python files for column errors.
    Check(CheckArgs),
    /// Suggest fixes for DataFrames the checker could not type (dry run: prints diffs, changes nothing).
    Suggest(SuggestArgs),
    /// Print a bug-report body for one untyped site, with a redacted copy of its code (review before posting).
    Report(ReportArgs),
    /// Find, clear or gitignore the on-disk index cache.
    #[command(long_about = CACHE_HELP)]
    Cache(CacheArgs),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub(crate) enum OutputFormat {
    Text,
    Json,
    Github,
}

/// Cache options shared by every command that builds an index.
#[derive(Args, Debug, Default)]
pub(crate) struct CacheOptions {
    /// Do not read or write the on-disk index cache for this run.
    #[arg(long)]
    pub(crate) no_cache: bool,
    /// Keep the on-disk index cache here (default: the per-user cache directory; see `typedframes cache --help` for every way to set it).
    #[arg(long)]
    pub(crate) cache_dir: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub(crate) struct CheckArgs {
    /// File or directory to check (default: current directory).
    #[arg(default_value = ".")]
    pub(crate) path: PathBuf,
    /// Exit with code 1 if any errors are found.
    #[arg(long)]
    pub(crate) strict: bool,
    /// Output format: text (default), json, or github (GitHub Actions annotations).
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub(crate) output_format: OutputFormat,
    // Kept as a hidden alias for backward compatibility.
    #[arg(long, hide = true)]
    pub(crate) json: bool,
    /// Disable the cross-file index entirely; check every file in isolation, following no imports.
    #[arg(long)]
    pub(crate) no_index: bool,
    #[command(flatten)]
    pub(crate) cache: CacheOptions,
    /// Suppress all warnings (dropped-unknown-column and untracked-dataframe).
    #[arg(long)]
    pub(crate) no_warnings: bool,
    /// Deprecated, no-op: untracked-dataframe is a warning-level diagnostic by default now.
    #[arg(long)]
    pub(crate) strict_ingest: bool,
    /// Downgrade untracked-dataframe from a warning-level diagnostic back to info-level for bare DataFrame loads without usecols= or columns=.
    #[arg(long)]
    pub(crate) lenient_ingest: bool,
    /// Suppress informational output: the DataFrame schema coverage summary and info-level diagnostics.
    #[arg(long)]
    pub(crate) no_info: bool,
    /// Enforce a minimum DataFrame schema coverage: exit 1 if fewer than N% of DataFrames have recognized column info. A total override: it applies one threshold everywhere and ignores [tool.typedframes.coverage] entirely, per-path overrides included.
    #[arg(long = "coverage-fail-under", value_name = "N", value_parser = parse_percentage)]
    pub(crate) fail_under: Option<f64>,
    /// How much DataFrame schema coverage detail to print: summary (default, one line), term-missing (per-file table plus the DataFrame sites lacking column info), or explain (every DataFrame-shaped call found in each file, whether or not it was actually counted, and why not -- a diagnostic for 'why is my coverage lower than expected'). Combine with --output-format=json to get the same detail as structured JSON, nested under a 'coverage' key, instead of text. Overrides the `detail` key in [tool.typedframes.coverage].
    #[arg(long, value_enum)]
    pub(crate) coverage_detail: Option<CoverageDetail>,
}

impl CheckArgs {
    /// The output format, with the hidden `--json` alias applied.
    pub(crate) fn format(&self) -> OutputFormat {
        if self.json {
            OutputFormat::Json
        } else {
            self.output_format
        }
    }
}

#[cfg(test)]
impl CheckArgs {
    /// Default `check` arguments, adjusted by `adjust`.
    pub(crate) fn for_test(adjust: impl FnOnce(&mut CheckArgs)) -> CheckArgs {
        let Some(Command::Check(mut args)) = Cli::try_parse_from(["typedframes", "check"])
            .expect("a bare check command parses")
            .command
        else {
            unreachable!("`check` parses to a check command")
        };
        adjust(&mut args);
        args
    }
}

fn parse_percentage(value: &str) -> Result<f64, String> {
    let pct: f64 = value
        .trim()
        .parse()
        .map_err(|_| format!("invalid percentage: {value:?}"))?;
    if !(0.0..=100.0).contains(&pct) {
        return Err(format!("percentage must be between 0 and 100, got {pct:?}"));
    }
    Ok(pct)
}

#[derive(Args, Debug)]
pub(crate) struct SuggestArgs {
    /// File or directory to suggest fixes for (default: current directory).
    #[arg(default_value = ".")]
    pub(crate) path: PathBuf,
    /// Disable the cross-file index entirely; look at every file in isolation, following no imports.
    #[arg(long)]
    pub(crate) no_index: bool,
    #[command(flatten)]
    pub(crate) cache: CacheOptions,
    /// Apply every `safe` suggestion to the files (review-tier ones are never applied this way).
    #[arg(long, conflicts_with = "interactive")]
    pub(crate) apply: bool,
    /// Show each suggestion and ask whether to apply it (y/n/a to apply this and the rest/q to quit).
    #[arg(short, long)]
    pub(crate) interactive: bool,
    /// Act on just the site with this two-word name (shown next to each site; repeat for several). With --apply it is applied even if review-tier, since naming it is the choice.
    #[arg(long, value_name = "NAME")]
    pub(crate) only: Vec<String>,
    /// With --apply or --interactive, edit files even if the git work tree has uncommitted changes or there is no git repository.
    #[arg(long)]
    pub(crate) allow_dirty: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub(crate) enum Outcome {
    Suggested,
    Withheld,
    /// Frames used like a DataFrame that the checker does not track at all.
    Untracked,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub(crate) enum ReportFormat {
    Markdown,
    Json,
    Compact,
}

#[derive(Args, Debug)]
pub(crate) struct ReportArgs {
    /// The site's two-word name, as shown by `suggest`.
    pub(crate) name: Option<String>,
    /// File or directory to look in (default: current directory).
    #[arg(default_value = ".")]
    pub(crate) path: PathBuf,
    /// Report every untyped site, grouped by why the checker decided as it did. Takes PATH, not NAME.
    #[arg(long)]
    pub(crate) all: bool,
    /// With --all, only the sites the checker decided this way about. `untracked` lists instead the frames used like a DataFrame that the checker does not track, which are in no other report.
    #[arg(long, value_enum)]
    pub(crate) outcome: Option<Outcome>,
    /// With --all, how to print the groups (default: markdown).
    #[arg(long = "format", value_enum, default_value_t = ReportFormat::Markdown)]
    pub(crate) report_format: ReportFormat,
    /// With --all, write one bug-report body per group into this directory instead of printing.
    #[arg(long)]
    pub(crate) output_dir: Option<PathBuf>,
    /// Look at every file in isolation.
    #[arg(long)]
    pub(crate) no_index: bool,
    #[command(flatten)]
    pub(crate) cache: CacheOptions,
}

#[derive(Args, Debug)]
pub(crate) struct CacheArgs {
    /// path: print the cache directory. clear: delete the cache files. gitignore: add the cache directory to the project's .gitignore.
    #[arg(value_enum)]
    pub(crate) action: CacheAction,
    /// A file or directory in the project whose cache to act on (default: the current directory).
    #[arg(default_value = ".")]
    pub(crate) path: PathBuf,
    /// Use this directory.
    #[arg(long)]
    pub(crate) cache_dir: Option<PathBuf>,
}

/// Parses `args` (program name first). A bad command line, `--help` and `--version` come back as
/// the clap error for the caller to print, so nothing here ends the process.
pub(crate) fn parse_from<I, T>(args: I) -> Result<Cli, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    Cli::try_parse_from(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(args: &[&str]) -> CheckArgs {
        let mut argv = vec!["typedframes", "check"];
        argv.extend_from_slice(args);
        match Cli::try_parse_from(argv).unwrap().command {
            Some(Command::Check(check)) => check,
            _ => panic!("not a check command"),
        }
    }

    #[test]
    fn test_should_default_check_to_the_current_directory_and_text_output() {
        // act
        let args = check(&[]);

        // assert
        assert_eq!(PathBuf::from("."), args.path);
        assert_eq!(OutputFormat::Text, args.format());
        assert!(!args.strict);
    }

    #[test]
    fn test_should_treat_the_hidden_json_flag_as_json_output() {
        assert_eq!(OutputFormat::Json, check(&["--json"]).format());
    }

    #[test]
    fn test_should_accept_a_percentage_in_range_and_reject_one_outside() {
        assert_eq!(
            Some(87.5),
            check(&["--coverage-fail-under", "87.5"]).fail_under
        );
        assert!(
            Cli::try_parse_from(["typedframes", "check", "--coverage-fail-under", "101"]).is_err()
        );
        assert!(
            Cli::try_parse_from(["typedframes", "check", "--coverage-fail-under", "x"]).is_err()
        );
    }

    #[test]
    fn test_should_refuse_apply_together_with_interactive() {
        let parsed = Cli::try_parse_from(["typedframes", "suggest", "--apply", "-i"]);

        assert!(parsed.is_err());
    }

    #[test]
    fn test_should_collect_repeated_only_names() {
        // act
        let parsed = Cli::try_parse_from([
            "typedframes",
            "suggest",
            "--only",
            "quiet-amber",
            "--only",
            "bold-fox",
        ])
        .unwrap();

        // assert
        let Some(Command::Suggest(args)) = parsed.command else {
            panic!("not a suggest command");
        };
        assert_eq!(vec!["quiet-amber", "bold-fox"], args.only);
    }

    #[test]
    fn test_should_take_a_cache_action_and_an_optional_path() {
        let parsed = Cli::try_parse_from(["typedframes", "cache", "path"]).unwrap();

        let Some(Command::Cache(args)) = parsed.command else {
            panic!("not a cache command");
        };
        assert_eq!(CacheAction::Path, args.action);
        assert_eq!(PathBuf::from("."), args.path);
    }
}
