//! `typedframes suggest`: print suggestions as diffs, or apply them.

use super::args::SuggestArgs;
use super::check::{build_index, resolve_existing_path};
use super::collect::collect_files;
use super::config::load_configured_excludes;
use super::pathutil::{posix, relative_posix, resolve};
use super::pyfmt::{dumps_str, repr_str};
use crate::apply::{apply_all, ApplyResult, Selection};
use crate::engine::suggest_batch;
use crate::index::ProjectIndex;
use serde_json::Value;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::sync::Arc;

mod choose;
mod outcome;
mod present;

use choose::*;
use outcome::*;
pub(crate) use present::*;

/// A site (a suggestion or a withheld one) and the file it is in.
type Site = (String, Value);

struct Collected {
    suggestions: Vec<Site>,
    withheld: Vec<Site>,
    unreadable: usize,
}

type Chosen = Vec<(String, Vec<(usize, usize)>)>;

type Request = (String, String, Selection);

/// Runs `suggest`: prints suggestions, or applies them with `--apply` / `--interactive`.
/// Several paths are run one after another, each on its own.
pub(crate) fn run_suggest(
    args: &SuggestArgs,
    out: &mut impl Write,
    input: &mut impl BufRead,
) -> io::Result<i32> {
    let mut worst = 0;
    for path in &args.path {
        let code = run_suggest_path(path, args, out, input)?;
        worst = worst.max(code);
    }
    Ok(worst)
}

fn run_suggest_path(
    path: &Path,
    args: &SuggestArgs,
    out: &mut impl Write,
    input: &mut impl BufRead,
) -> io::Result<i32> {
    let path = match resolve_existing_path(path) {
        Ok(path) => path,
        Err(code) => return Ok(code),
    };
    let index = build_index(
        &path,
        args.no_index,
        args.cache.cache_dir.as_deref(),
        args.cache.no_cache,
        true,
    );
    let root = if path.is_dir() {
        path.clone()
    } else {
        path.parent().map(Path::to_path_buf).unwrap_or_default()
    };
    let excludes = load_configured_excludes(&path);
    let files = collect_files(&path, &[".py", ".ipynb"], excludes.as_ref());

    let editing = args.apply || args.interactive;
    if editing && !args.allow_dirty {
        if let Some(problem) = typedframes_adapters::vcs_problem(&root) {
            eprintln!("Error: {problem}");
            return Ok(1);
        }
    }

    let Collected {
        mut suggestions,
        mut withheld,
        unreadable,
    } = collect_suggestions(&files, &root, index.clone());
    if !args.only.is_empty() {
        match select_named_sites(&args.only, suggestions, withheld, editing) {
            Ok(selected) => (suggestions, withheld) = selected,
            Err(code) => return Ok(code),
        }
    }

    if !editing {
        print_suggestions(out, &suggestions, &withheld, files.len())?;
        return Ok(0);
    }
    let Some(requests) = edit_requests(out, input, args, &suggestions)? else {
        return Ok(0);
    };
    let failed = if requests.is_empty() {
        report_nothing_to_apply(
            out,
            suggestions.len(),
            withheld.len(),
            args.apply && !suggestions.is_empty(),
        )?;
        0
    } else {
        match apply_and_report(out, &requests, index, !args.allow_dirty)? {
            Ok(failed) => failed,
            Err(code) => return Ok(code),
        }
    };
    // Exit non-zero if an edit run left files unprocessed; a dry run just skips them.
    Ok(i32::from(unreadable > 0 || failed > 0))
}
