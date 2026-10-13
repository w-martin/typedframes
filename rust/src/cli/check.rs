//! `typedframes check`.

use super::args::{CheckArgs, OutputFormat};
use super::cache::{resolve_cache_dir, tighten_cache_permissions};
use super::collect::collect_files;
use super::config::{load_configured_excludes, load_coverage_config, CoverageDetail};
use super::coverage::{
    coverage_json, evaluate_coverage, explain_json, failure_message, format_explain,
    format_term_missing, Sources, Tally,
};
use super::pathutil::{repr, resolve};
use super::pyfmt::{dumps_indent2, round_half_even};
use super::style::{BOLD, BOLD_GREEN, BOLD_RED, BOLD_YELLOW, DIM, RESET};
use crate::engine::{check_batch, FileOutcome};
use crate::index::{
    build_index_cached, build_index_with_callers, build_single_file_index_cached, ProjectIndex,
};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

// Below this fraction of tracked DataFrames having a concrete column list, the summary leads with
// a coverage caveat instead of trailing it.
const LOW_COVERAGE_THRESHOLD: f64 = 0.5;

/// What the checker found across every file, each site tagged with its `file`.
#[derive(Default)]
pub(crate) struct Findings {
    pub(crate) errors: Vec<Value>,
    pub(crate) total: u64,
    pub(crate) typed: u64,
    pub(crate) per_file: BTreeMap<String, Tally>,
    pub(crate) untyped_sites: Vec<Value>,
    pub(crate) typed_sites: Vec<Value>,
    pub(crate) all_dataframe_calls: Vec<Value>,
    pub(crate) leg_events: Vec<Value>,
    pub(crate) untyped_site_usage: Vec<Value>,
}

// `{**item, "file": file}`: the file goes last unless the item already had one.
fn tagged(item: &Value, file: &str) -> Value {
    let mut tagged = item.clone();
    if let Some(map) = tagged.as_object_mut() {
        map.insert("file".into(), Value::String(file.to_string()));
    }
    tagged
}

fn tagged_all(stats: &Value, key: &str, file: &str) -> Vec<Value> {
    stats
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|item| tagged(item, file))
        .collect()
}

/// Folds each file's outcome into [`Findings`]. A file the checker skipped (unreadable, not valid
/// Python, a notebook that isn't Python) is named on stderr and the run carries on.
pub(crate) fn gather(files: &[PathBuf], outcomes: Vec<FileOutcome>) -> Findings {
    let mut findings = Findings::default();
    for (file, (result, error)) in files.iter().zip(outcomes) {
        let name = file.to_string_lossy().into_owned();
        let Some(parsed) = result
            .as_deref()
            .and_then(|json| serde_json::from_str::<Value>(json).ok())
        else {
            eprintln!("{name}: skipped, {}", error.as_deref().unwrap_or("None"));
            continue;
        };
        for error in parsed["errors"].as_array().into_iter().flatten() {
            findings.errors.push(tagged(error, &name));
        }
        let stats = &parsed["stats"];
        let tally = Tally {
            total: stats["dataframes_total"].as_u64().unwrap_or(0),
            typed: stats["dataframes_typed"].as_u64().unwrap_or(0),
        };
        findings.total += tally.total;
        findings.typed += tally.typed;
        findings.per_file.insert(name.clone(), tally);
        findings
            .untyped_sites
            .extend(tagged_all(stats, "untyped_sites", &name));
        findings
            .typed_sites
            .extend(tagged_all(stats, "typed_sites", &name));
        findings
            .all_dataframe_calls
            .extend(tagged_all(stats, "all_dataframe_calls", &name));
        findings
            .leg_events
            .extend(tagged_all(stats, "leg_events", &name));
        findings
            .untyped_site_usage
            .extend(tagged_all(stats, "untyped_site_usage", &name));
    }
    findings
}

impl Findings {
    fn sources(&self) -> Sources<'_> {
        Sources {
            all_dataframe_calls: &self.all_dataframe_calls,
            typed_sites: &self.typed_sites,
            untyped_sites: &self.untyped_sites,
            leg_events: &self.leg_events,
            untyped_site_usage: &self.untyped_site_usage,
        }
    }
}

fn severity(error: &Value) -> Option<&str> {
    error.get("severity").and_then(Value::as_str)
}

fn str_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn int_of(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(0)
}

// `line:col`, or `cell N:line:col` for a notebook error.
fn error_location(error: &Value) -> String {
    let prefix = error
        .get("cell")
        .and_then(Value::as_i64)
        .map(|cell| format!("cell {cell}:"))
        .unwrap_or_default();
    format!("{prefix}{}:{}", int_of(error, "line"), int_of(error, "col"))
}

/// Errors as `file:line:col: severity[code] message` lines, in colour when asked.
pub(crate) fn format_text(errors: &[Value], color: bool) -> String {
    errors
        .iter()
        .map(|error| {
            let severity = severity(error).unwrap_or("error");
            let code = str_of(error, "code");
            let file = str_of(error, "file");
            let location = error_location(error);
            let message = str_of(error, "message");
            let code_part = if code.is_empty() {
                String::new()
            } else {
                format!("[{code}]")
            };
            if color {
                let coloured = match severity {
                    "error" => format!("{BOLD_RED}error{RESET}"),
                    "warning" => format!("{BOLD_YELLOW}warning{RESET}"),
                    _ => format!("{DIM}info{RESET}"),
                };
                format!("{BOLD}{file}{RESET}:{location}: {coloured}{code_part} {message}")
            } else {
                format!("{file}:{location}: {severity}{code_part} {message}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Errors as GitHub Actions workflow commands. Only error/warning/notice are recognised, so
/// "info" becomes "notice". A notebook has no real line numbers, so its cell goes in the title.
pub(crate) fn format_github(errors: &[Value]) -> String {
    errors
        .iter()
        .map(|error| {
            let severity = severity(error).unwrap_or("error");
            let github = match severity {
                "info" => "notice",
                other => other,
            };
            let code = str_of(error, "code");
            let mut title = if code.is_empty() { severity } else { code }.to_string();
            if let Some(cell) = error.get("cell").and_then(Value::as_i64) {
                title = format!("{title} (notebook cell {cell})");
            }
            format!(
                "::{github} file={},line={},col={},title={title}::{}",
                str_of(error, "file"),
                int_of(error, "line"),
                int_of(error, "col"),
                str_of(error, "message")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn error_summary_parts(errors_only: &[&Value], warnings: usize) -> Vec<String> {
    let unverifiable = errors_only
        .iter()
        .filter(|e| str_of(e, "code") == "unverifiable-column")
        .count();
    let confirmed = errors_only.len() - unverifiable;
    let mut parts = Vec::new();
    // Confirmed and unverifiable only get two phrases when there is an unverifiable one, so the
    // common case reads as it always has ("N errors").
    if confirmed > 0 || unverifiable == 0 {
        let label = if confirmed == 1 { "error" } else { "errors" };
        parts.push(format!("{confirmed} {label}"));
    }
    if unverifiable > 0 {
        let label = if unverifiable == 1 {
            "unverifiable error"
        } else {
            "unverifiable errors"
        };
        parts.push(format!("{unverifiable} {label}"));
    }
    if warnings > 0 {
        let label = if warnings == 1 { "warning" } else { "warnings" };
        parts.push(format!("{warnings} {label}"));
    }
    parts
}

/// A line noting the diagnostics below only reflect what little could be tracked: printed first
/// when under half the DataFrames the checker saw had a concrete column list. `None` when there is
/// nothing below it to caveat.
pub(crate) fn low_coverage_caveat(total: u64, typed: u64, printed: usize) -> Option<String> {
    if printed == 0 || total == 0 || typed as f64 / total as f64 >= LOW_COVERAGE_THRESHOLD {
        return None;
    }
    let pct = round_half_even(100.0 * typed as f64 / total as f64);
    let (noun, verb) = if printed == 1 {
        ("finding", "covers")
    } else {
        ("findings", "cover")
    };
    Some(format!(
        "\u{2139} {typed}/{total} DataFrames had column info ({pct}%) \u{2014} the {printed} {noun} below only {verb} what little could be tracked"
    ))
}

/// The low-key coverage summary line: how much information the checker had, not a validation
/// result.
pub(crate) fn coverage_message(total: u64, typed: u64) -> String {
    if total == 0 {
        return "\u{2139} No DataFrames with recognized loads/schemas found to check".to_string();
    }
    let pct = round_half_even(100.0 * typed as f64 / total as f64);
    format!(
        "\u{2139} {typed}/{total} DataFrames had column info ({pct}%) \u{2014} DataFrame schema coverage, not a pass/fail result"
    )
}

/// A duration in whichever of µs, ms or s keeps it readable, switching units after rounding.
pub(crate) fn format_duration(seconds: f64) -> String {
    let micros = round_half_even(seconds / 1e-6);
    if micros < 1000 {
        return format!("{micros}\u{b5}s");
    }
    let millis = round_half_even(seconds / 1e-3);
    if millis < 1000 {
        return format!("{millis}ms");
    }
    format!("{seconds:.2}s")
}

// What the run produced, for the summary and the JSON document.
struct Run {
    files: usize,
    elapsed: f64,
    total: u64,
    typed: u64,
}

fn drop_after_chars(text: &str, n: usize) -> &str {
    text.char_indices().nth(n).map_or("", |(i, _)| &text[i..])
}

fn print_results(
    out: &mut impl Write,
    run: &Run,
    errors: &[Value],
    format: OutputFormat,
    show_info: bool,
) -> io::Result<()> {
    let errors_only: Vec<&Value> = errors
        .iter()
        .filter(|e| !matches!(severity(e), Some("warning" | "info")))
        .collect();
    let warnings = errors
        .iter()
        .filter(|e| severity(e) == Some("warning"))
        .count();
    let color = format == OutputFormat::Text && io::stdout().is_terminal();
    let caveat = if show_info {
        low_coverage_caveat(run.total, run.typed, errors.len())
    } else {
        None
    };

    if format == OutputFormat::Github {
        if let Some(caveat) = &caveat {
            writeln!(
                out,
                "::notice title=typedframes DataFrame schema coverage::{}",
                drop_after_chars(caveat, 2)
            )?;
        }
        if !errors.is_empty() {
            writeln!(out, "{}", format_github(errors))?;
        }
        if show_info && caveat.is_none() {
            writeln!(
                out,
                "::notice title=typedframes DataFrame schema coverage::{}",
                drop_after_chars(&coverage_message(run.total, run.typed), 2)
            )?;
        }
        return Ok(());
    }

    let paint = |style: &str, text: &str| {
        if color {
            format!("{style}{text}{RESET}")
        } else {
            text.to_string()
        }
    };
    if let Some(caveat) = &caveat {
        writeln!(out, "{}", paint(DIM, caveat))?;
    }
    if !errors.is_empty() {
        writeln!(out, "{}", format_text(errors, color))?;
        writeln!(out)?;
    }
    let label = if run.files == 1 { "file" } else { "files" };
    let duration = format_duration(run.elapsed);
    if !errors_only.is_empty() || warnings > 0 {
        let summary = error_summary_parts(&errors_only, warnings).join(", ");
        let message = format!(
            "\u{2717} Found {summary} in {} {label} ({duration})",
            run.files
        );
        writeln!(out, "{}", paint(BOLD_RED, &message))?;
    } else {
        let message = format!("\u{2713} Checked {} {label} in {duration}", run.files);
        writeln!(out, "{}", paint(BOLD_GREEN, &message))?;
    }
    if show_info && caveat.is_none() {
        writeln!(
            out,
            "{}",
            paint(DIM, &coverage_message(run.total, run.typed))
        )?;
    }
    Ok(())
}

fn print_json_results(
    out: &mut impl Write,
    run: &Run,
    errors: Vec<Value>,
    coverage: Option<Value>,
) -> io::Result<()> {
    let mut payload = Map::new();
    payload.insert("errors".into(), Value::Array(errors));
    payload.insert(
        "stats".into(),
        json!({"dataframes_total": run.total, "dataframes_typed": run.typed}),
    );
    if let Some(coverage) = coverage {
        payload.insert("coverage".into(), coverage);
    }
    writeln!(out, "{}", dumps_indent2(&Value::Object(payload)))
}

// The richer report `--coverage-detail` asked for, for text and GitHub output; under JSON the same
// detail is nested into the single document instead.
fn print_coverage_report(
    out: &mut impl Write,
    findings: &Findings,
    root: &Path,
    detail: CoverageDetail,
) -> io::Result<()> {
    if detail == CoverageDetail::Summary {
        return Ok(());
    }
    writeln!(out)?;
    if detail == CoverageDetail::Explain {
        writeln!(out, "{}", format_explain(&findings.sources(), root))
    } else {
        writeln!(
            out,
            "{}",
            format_term_missing(&findings.per_file, &findings.untyped_sites, root)
        )
    }
}

// Applies `--lenient-ingest` (untracked-dataframe downgraded to info) and `--no-warnings` /
// `--no-info` filtering. The checker already reports untracked-dataframe as a warning.
fn apply_diagnostic_policy(mut errors: Vec<Value>, args: &CheckArgs) -> Vec<Value> {
    if args.lenient_ingest {
        for error in &mut errors {
            if str_of(error, "code") == "untracked-dataframe" {
                if let Some(map) = error.as_object_mut() {
                    map.insert("severity".into(), Value::String("info".into()));
                }
            }
        }
    }
    if args.no_warnings {
        errors.retain(|e| severity(e) != Some("warning"));
    }
    if args.no_info {
        errors.retain(|e| severity(e) != Some("info"));
    }
    errors
}

/// The index for this run, unless `--no-index` disabled it: the whole project for a directory
/// (taking `path` as the project root), or one scoped to a single file's own references, found from
/// the nearest `pyproject.toml` above it (`None` when there is none).
///
/// `with_callers` also records what every call to a function does with its result, which costs one
/// more read of every file, so only the commands that narrow a returned frame ask.
pub(crate) fn build_index(
    path: &Path,
    no_index: bool,
    cache_dir: Option<&Path>,
    no_cache: bool,
    with_callers: bool,
) -> Option<Arc<ProjectIndex>> {
    if no_index {
        return None;
    }
    let cache_dir = resolve_cache_dir(path, cache_dir, no_cache);
    if let Some(dir) = &cache_dir {
        tighten_cache_permissions(dir);
    }
    if path.is_dir() {
        let build = if with_callers {
            build_index_with_callers
        } else {
            build_index_cached
        };
        Some(Arc::new(build(path, cache_dir.as_deref())))
    } else {
        build_single_file_index_cached(path, cache_dir.as_deref()).map(Arc::new)
    }
}

/// Resolves a command's path argument; a path that doesn't exist is reported and exit status 2.
pub(crate) fn resolve_existing_path(original: &Path) -> Result<PathBuf, i32> {
    let path = resolve(original);
    if path.exists() {
        return Ok(path);
    }
    if original.is_absolute() {
        eprintln!("Error: path does not exist: {}", path.display());
    } else {
        eprintln!(
            "Error: path does not exist: {} (resolved to {})",
            repr(original),
            path.display()
        );
    }
    Err(2)
}

fn print_coverage_failures(
    out: &mut impl Write,
    failing: &[super::coverage::Bucket],
    format: OutputFormat,
) -> io::Result<()> {
    for bucket in failing {
        let message = failure_message(bucket);
        match format {
            OutputFormat::Github => writeln!(
                out,
                "::error title=typedframes DataFrame schema coverage::{}",
                drop_after_chars(&message, 2)
            )?,
            OutputFormat::Json => eprintln!("{message}"),
            OutputFormat::Text => {
                if io::stdout().is_terminal() {
                    writeln!(out, "{BOLD_RED}{message}{RESET}")?;
                } else {
                    writeln!(out, "{message}")?;
                }
            }
        }
    }
    Ok(())
}

/// Runs `check`, writing its report to `out`; returns the exit status.
pub(crate) fn run_check(args: &CheckArgs, out: &mut impl Write) -> io::Result<i32> {
    let start = Instant::now();
    let path = match resolve_existing_path(&args.path) {
        Ok(path) => path,
        Err(code) => return Ok(code),
    };

    // Loaded before the run so an unusable config is reported up front rather than after a long
    // check.
    let coverage_config = load_coverage_config(&path);
    let index = build_index(
        &path,
        args.no_index,
        args.cache.cache_dir.as_deref(),
        args.cache.no_cache,
        false,
    );
    let excludes = load_configured_excludes(&path);
    let files = collect_files(&path, &[".py", ".ipynb"], excludes.as_ref());
    let names: Vec<String> = files
        .iter()
        .map(|f| f.to_string_lossy().into_owned())
        .collect();
    let findings = gather(&files, check_batch(&names, index));
    let run = Run {
        files: files.len(),
        elapsed: start.elapsed().as_secs_f64(),
        total: findings.total,
        typed: findings.typed,
    };

    let errors = apply_diagnostic_policy(findings.errors.clone(), args);
    let errors_only = errors
        .iter()
        .filter(|e| !matches!(severity(e), Some("warning" | "info")))
        .count();
    // The flag wins over the config key, so a one-off `--coverage-detail` doesn't mean editing
    // project config.
    let detail = args.coverage_detail.unwrap_or(coverage_config.detail);
    let format = args.format();

    if format == OutputFormat::Json {
        let coverage = match detail {
            CoverageDetail::Summary => None,
            CoverageDetail::Explain => Some(explain_json(&findings.sources(), &path)),
            CoverageDetail::TermMissing => Some(coverage_json(
                &findings.per_file,
                &findings.untyped_sites,
                &path,
            )),
        };
        print_json_results(out, &run, errors, coverage)?;
    } else {
        print_results(out, &run, &errors, format, !args.no_info)?;
        print_coverage_report(out, &findings, &path, detail)?;
    }

    // Coverage is a separate gate from --strict: --strict judges correctness, the threshold judges
    // annotation completeness. Enforcing one has never implied the other.
    let mut failing = Vec::new();
    if args.fail_under.is_some() || coverage_config.enabled {
        failing = evaluate_coverage(&findings.per_file, &coverage_config, &path, args.fail_under);
        print_coverage_failures(out, &failing, format)?;
    }
    Ok(i32::from(
        !failing.is_empty() || (args.strict && errors_only > 0),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(file: &str, line: i64, severity: &str, code: &str) -> Value {
        json!({"line": line, "col": 5, "code": code, "message": "m", "severity": severity, "file": file})
    }

    #[test]
    fn test_should_format_an_error_as_a_ty_style_line() {
        // arrange
        let errors = vec![error("a.py", 3, "error", "unknown-column")];

        // act
        let text = format_text(&errors, false);

        // assert
        assert_eq!("a.py:3:5: error[unknown-column] m", text);
    }

    #[test]
    fn test_should_put_a_notebook_cell_before_the_position() {
        // arrange
        let mut error = error("n.ipynb", 3, "warning", "");
        error["cell"] = json!(2);

        // act
        let text = format_text(&[error], false);

        // assert
        assert_eq!("n.ipynb:cell 2:3:5: warning m", text);
    }

    #[test]
    fn test_should_colour_each_severity() {
        // arrange
        let errors = vec![error("a.py", 1, "info", "c")];

        // act
        let text = format_text(&errors, true);

        // assert
        assert_eq!(
            "\u{1b}[1ma.py\u{1b}[0m:1:5: \u{1b}[2minfo\u{1b}[0m[c] m",
            text
        );
    }

    #[test]
    fn test_should_map_info_to_a_github_notice_and_name_the_notebook_cell() {
        // arrange
        let mut info = error("a.py", 1, "info", "");
        let mut cell = error("n.ipynb", 2, "error", "unknown-column");
        cell["cell"] = json!(4);
        info["code"] = json!("");

        // act
        let text = format_github(&[info, cell]);

        // assert
        assert_eq!(
            "::notice file=a.py,line=1,col=5,title=info::m\n::error file=n.ipynb,line=2,col=5,title=unknown-column (notebook cell 4)::m",
            text
        );
    }

    #[test]
    fn test_should_count_unverifiable_errors_separately_only_when_there_are_some() {
        // arrange
        let plain = error("a.py", 1, "error", "unknown-column");
        let unverifiable = error("a.py", 2, "error", "unverifiable-column");

        // act
        let only_plain = error_summary_parts(&[&plain, &plain], 1);
        let mixed = error_summary_parts(&[&plain, &unverifiable, &unverifiable], 0);
        let only_unverifiable = error_summary_parts(&[&unverifiable], 0);

        // assert
        assert_eq!(vec!["2 errors", "1 warning"], only_plain);
        assert_eq!(vec!["1 error", "2 unverifiable errors"], mixed);
        assert_eq!(vec!["1 unverifiable error"], only_unverifiable);
    }

    #[test]
    fn test_should_lead_with_a_caveat_only_below_half_coverage_and_with_findings() {
        assert_eq!(None, low_coverage_caveat(10, 4, 0));
        assert_eq!(None, low_coverage_caveat(10, 5, 3));
        assert_eq!(None, low_coverage_caveat(0, 0, 3));
        assert_eq!(
            Some("\u{2139} 1/4 DataFrames had column info (25%) \u{2014} the 1 finding below only covers what little could be tracked".to_string()),
            low_coverage_caveat(4, 1, 1)
        );
    }

    #[test]
    fn test_should_word_the_coverage_summary() {
        assert_eq!(
            "\u{2139} No DataFrames with recognized loads/schemas found to check",
            coverage_message(0, 0)
        );
        assert_eq!(
            "\u{2139} 1/3 DataFrames had column info (33%) \u{2014} DataFrame schema coverage, not a pass/fail result",
            coverage_message(3, 1)
        );
    }

    #[test]
    fn test_should_switch_duration_units_after_rounding() {
        assert_eq!("999\u{b5}s", format_duration(0.000_999));
        assert_eq!("1ms", format_duration(0.000_999_9));
        assert_eq!("999ms", format_duration(0.999));
        assert_eq!("1.00s", format_duration(0.999_6));
    }

    #[test]
    fn test_should_tag_each_site_with_its_file_last() {
        // arrange
        let site = json!({"var": "df", "line": 3});

        // act
        let tagged = tagged(&site, "a.py");

        // assert
        let keys: Vec<&String> = tagged.as_object().unwrap().keys().collect();
        assert_eq!(vec!["var", "line", "file"], keys);
    }

    #[test]
    fn test_should_skip_a_file_the_checker_could_not_read_and_keep_the_rest() {
        // arrange
        let files = vec![PathBuf::from("a.py"), PathBuf::from("b.py")];
        let good = json!({"errors": [{"line": 1, "col": 1, "code": "c", "message": "m", "severity": "error"}],
                          "stats": {"dataframes_total": 2, "dataframes_typed": 1}})
        .to_string();
        let outcomes = vec![
            (None, Some("permission denied".to_string())),
            (Some(good), None),
        ];

        // act
        let findings = gather(&files, outcomes);

        // assert
        assert_eq!(1, findings.errors.len());
        assert_eq!("b.py", findings.errors[0]["file"]);
        assert_eq!((2, 1), (findings.total, findings.typed));
        assert_eq!(1, findings.per_file.len());
    }

    #[test]
    fn test_should_downgrade_untracked_dataframe_to_info_and_filter_by_flag() {
        // arrange
        let errors = vec![
            error("a.py", 1, "warning", "untracked-dataframe"),
            error("a.py", 2, "warning", "dropped-unknown-column"),
            error("a.py", 3, "error", "unknown-column"),
        ];
        let lenient = CheckArgs::for_test(|a| a.lenient_ingest = true);
        let quiet = CheckArgs::for_test(|a| {
            a.lenient_ingest = true;
            a.no_warnings = true;
            a.no_info = true;
        });

        // act
        let downgraded = apply_diagnostic_policy(errors.clone(), &lenient);
        let filtered = apply_diagnostic_policy(errors, &quiet);

        // assert
        assert_eq!(Some("info"), severity(&downgraded[0]));
        assert_eq!(1, filtered.len());
        assert_eq!(Some("error"), severity(&filtered[0]));
    }
}
