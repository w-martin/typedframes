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
use typedframes_adapters::{DiskIndexCache, OsFiles};
use typedframes_domain::ports::IndexCache;

mod findings;
mod format;
mod output;
mod policy;

pub(crate) use findings::*;
pub(crate) use format::*;
use output::*;
use policy::*;

// Below this fraction of tracked DataFrames having a concrete column list, the summary leads with
// a coverage caveat instead of trailing it.
const LOW_COVERAGE_THRESHOLD: f64 = 0.5;

/// What the checker found across every file, each site tagged with its `file`.
#[derive(Default)]
pub(crate) struct Findings {
    pub(crate) errors: Vec<Value>,
    pub(crate) total: u64,
    pub(crate) typed: u64,
    pub(crate) unresolved: u64,
    pub(crate) per_file: BTreeMap<String, Tally>,
    pub(crate) untyped_sites: Vec<Value>,
    pub(crate) typed_sites: Vec<Value>,
    pub(crate) all_dataframe_calls: Vec<Value>,
    pub(crate) leg_events: Vec<Value>,
    pub(crate) untyped_site_usage: Vec<Value>,
}

// What the run produced, for the summary and the JSON document.
struct Run {
    files: usize,
    elapsed: f64,
    total: u64,
    typed: u64,
    unresolved: u64,
    // Frames derived from a tracked one whose columns the checker lost along the way.
    lost: u64,
    // Load calls no site accounts for.
    uncounted: u64,
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
    let cache = cache_dir.map(DiskIndexCache::new);
    let cache = cache.as_ref().map(|cache| cache as &dyn IndexCache);
    if path.is_dir() {
        let build = if with_callers {
            build_index_with_callers
        } else {
            build_index_cached
        };
        Some(Arc::new(build(&OsFiles, path, cache)))
    } else {
        build_single_file_index_cached(&OsFiles, path, cache).map(Arc::new)
    }
}

/// Resolves a command's path argument; `Err` is the message for a path that doesn't exist.
fn resolve_or_explain(original: &Path) -> Result<PathBuf, String> {
    let path = resolve(original);
    if path.exists() {
        return Ok(path);
    }
    if original.is_absolute() {
        Err(format!("Error: path does not exist: {}", path.display()))
    } else {
        Err(format!(
            "Error: path does not exist: {} (resolved to {})",
            repr(original),
            path.display()
        ))
    }
}

/// Resolves a command's path argument; a path that doesn't exist is reported and exit status 2.
pub(crate) fn resolve_existing_path(original: &Path) -> Result<PathBuf, i32> {
    resolve_or_explain(original).map_err(|message| {
        eprintln!("{message}");
        2
    })
}

/// Runs `check`, writing its report to `out` and the reason for a path that doesn't exist to `err`;
/// returns the exit status.
pub(crate) fn run_check(
    args: &CheckArgs,
    out: &mut impl Write,
    err: &mut impl Write,
) -> io::Result<i32> {
    let start = Instant::now();
    let path = match resolve_or_explain(&args.path) {
        Ok(path) => path,
        Err(message) => {
            writeln!(err, "{message}")?;
            return Ok(2);
        }
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
    let findings = gather(&files, check_batch(&crate::files(), &names, index));
    let run = Run {
        files: files.len(),
        elapsed: start.elapsed().as_secs_f64(),
        total: findings.total,
        typed: findings.typed,
        unresolved: findings.unresolved,
        lost: findings.lost_frames(),
        uncounted: findings.uncounted_loads(),
    };

    let errors = apply_diagnostic_policy(findings.errors.clone(), args);
    let errors_only = errors
        .iter()
        .filter(|e| !matches!(severity(e), Some("warning" | "info")))
        .count();
    // The flag wins over the config key, so a one-off `--coverage-detail` doesn't mean editing
    // project config.
    // Counted before --no-warnings hides the warnings: that flag silences output, and --strict
    // must still fail on a frame the checker could not look inside (a schema it could not
    // resolve, a column it could not verify): those are warnings, not errors, since not
    // knowing is not being wrong, but strict is the mode that asks for everything known.
    let unresolved_schemas = findings
        .errors
        .iter()
        .filter(|e| e["code"] == "unresolved-schema" || e["code"] == "unverifiable-column")
        .count();
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
        !failing.is_empty() || (args.strict && (errors_only > 0 || unresolved_schemas > 0)),
    ))
}

#[cfg(test)]
mod tests;
