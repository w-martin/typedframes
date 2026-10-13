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

/// A site (a suggestion or a withheld one) and the file it is in.
type Site = (String, Value);

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn int(value: &Value, key: &str) -> usize {
    value.get(key).and_then(Value::as_u64).unwrap_or(0) as usize
}

pub(crate) fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        format!("{count} {word}")
    } else {
        format!("{count} {word}s")
    }
}

// Name for a diff header: relative to the working directory so `git apply` accepts it, else to
// `root`.
fn diff_name(file: &Path, root: &Path) -> String {
    let cwd = resolve(&std::env::current_dir().unwrap_or_default());
    match file.strip_prefix(&cwd) {
        Ok(relative) => posix(relative),
        Err(_) => relative_posix(&file.to_string_lossy(), root),
    }
}

/// One suggestion: where, why, any review notes, then the diff the checker built.
pub(crate) fn format_suggestion(display_path: &str, suggestion: &Value) -> String {
    let columns = suggestion["columns"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|column| dumps_str(column.as_str().unwrap_or("")))
        .collect::<Vec<_>>()
        .join(", ");
    let where_ = format!(
        "{}  {display_path}:{}:{}",
        text(suggestion, "phrase"),
        int(suggestion, "line"),
        int(suggestion, "col")
    );
    let mut lines = vec![
        format!(
            "{where_}  {}  {} [{columns}]  [{}]",
            text(suggestion, "var"),
            text(suggestion, "kind"),
            text(suggestion, "tier")
        ),
        format!("  {}", text(suggestion, "reason")),
    ];
    for note in suggestion["notes"].as_array().into_iter().flatten() {
        lines.push(format!("  note: {}", note.as_str().unwrap_or("")));
    }
    lines.push(String::new());
    lines.push(text(suggestion, "diff").trim_end_matches('\n').to_string());
    lines.join("\n")
}

fn print_suggestions(
    out: &mut impl Write,
    suggestions: &[Site],
    withheld: &[Site],
    file_count: usize,
) -> io::Result<()> {
    if suggestions.is_empty() && withheld.is_empty() {
        return writeln!(out, "No untyped DataFrame sites found");
    }
    for (display_path, suggestion) in suggestions {
        writeln!(out, "{}", format_suggestion(display_path, suggestion))?;
        writeln!(out)?;
    }
    if !withheld.is_empty() {
        writeln!(out, "Not suggested:")?;
        for (display_path, site) in withheld {
            writeln!(
                out,
                "  {}  {display_path}:{}:{}  {}  {}",
                text(site, "phrase"),
                int(site, "line"),
                int(site, "col"),
                text(site, "var"),
                text(site, "reason")
            )?;
        }
        writeln!(out)?;
    }
    let safe = suggestions
        .iter()
        .filter(|(_, s)| text(s, "tier") == "safe")
        .count();
    writeln!(
        out,
        "{} ({safe} safe, {} review), {} withheld, in {}. Dry run: no files were changed.",
        plural(suggestions.len(), "suggestion"),
        suggestions.len() - safe,
        withheld.len(),
        plural(file_count, "file")
    )?;
    writeln!(
        out,
        "Each site has a name: use --only NAME to act on one, or `typedframes report NAME` to report a problem."
    )
}

struct Collected {
    suggestions: Vec<Site>,
    withheld: Vec<Site>,
    unreadable: usize,
}

fn collect_suggestions(
    files: &[std::path::PathBuf],
    root: &Path,
    index: Option<Arc<ProjectIndex>>,
) -> Collected {
    let requests: Vec<(String, String)> = files
        .iter()
        .map(|file| (file.to_string_lossy().into_owned(), diff_name(file, root)))
        .collect();
    let mut collected = Collected {
        suggestions: Vec::new(),
        withheld: Vec::new(),
        unreadable: 0,
    };
    for ((display, _), (result, error)) in requests.iter().zip(suggest_batch(&requests, index)) {
        let Some(parsed) = result
            .as_deref()
            .and_then(|j| serde_json::from_str::<Value>(j).ok())
        else {
            // Unreadable, or not valid Python.
            eprintln!("{display}: skipped, {}", error.as_deref().unwrap_or("None"));
            collected.unreadable += 1;
            continue;
        };
        for suggestion in parsed["suggestions"].as_array().into_iter().flatten() {
            collected
                .suggestions
                .push((display.clone(), suggestion.clone()));
        }
        for site in parsed["withheld"].as_array().into_iter().flatten() {
            collected.withheld.push((display.clone(), site.clone()));
        }
    }
    collected
}

// Keeps only the sites called `names`. Exits 2 if a name matches nothing or more than one site,
// and 1 if `editing` and a name is a site that has no suggestion.
fn select_named_sites(
    names: &[String],
    suggestions: Vec<Site>,
    withheld: Vec<Site>,
    editing: bool,
) -> Result<(Vec<Site>, Vec<Site>), i32> {
    let mut chosen_suggestions = Vec::new();
    let mut chosen_withheld = Vec::new();
    let mut seen: Vec<&String> = Vec::new();
    for raw in names {
        if seen.contains(&raw) {
            continue;
        }
        seen.push(raw);
        let name = raw.trim().to_lowercase();
        let found_suggestions: Vec<&Site> = suggestions
            .iter()
            .filter(|(_, s)| text(s, "phrase") == name)
            .collect();
        let found_withheld: Vec<&Site> = withheld
            .iter()
            .filter(|(_, w)| text(w, "phrase") == name)
            .collect();
        let total = found_suggestions.len() + found_withheld.len();
        if total == 0 {
            eprintln!("Error: no untyped site is named {}", repr_str(raw));
            return Err(2);
        }
        if total > 1 {
            eprintln!(
                "Error: {} names {total} sites; narrow the path to one of them:",
                repr_str(raw)
            );
            for (display_path, site) in found_suggestions.iter().chain(&found_withheld) {
                eprintln!(
                    "  {display_path}:{}:{}  {}",
                    int(site, "line"),
                    int(site, "col"),
                    text(site, "var")
                );
            }
            return Err(2);
        }
        if editing {
            if let Some((_, site)) = found_withheld.first() {
                eprintln!(
                    "Error: {} can't be edited: {}",
                    text(site, "phrase"),
                    text(site, "reason")
                );
                return Err(1);
            }
        }
        chosen_suggestions.extend(found_suggestions.into_iter().cloned());
        chosen_withheld.extend(found_withheld.into_iter().cloned());
    }
    Ok((chosen_suggestions, chosen_withheld))
}

// Reads one of y/n/a/q, asking again on anything else; end of input counts as `q`.
fn ask(out: &mut impl Write, input: &mut impl BufRead, prompt: &str) -> io::Result<String> {
    loop {
        write!(out, "{prompt}")?;
        out.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok("q".to_string());
        }
        let answer = line.trim().to_lowercase();
        if matches!(answer.as_str(), "y" | "n" | "a" | "q") {
            return Ok(answer);
        }
        writeln!(out, "Please answer y, n, a or q.")?;
    }
}

type Chosen = Vec<(String, Vec<(usize, usize)>)>;

fn choose_for(chosen: &mut Chosen, display: &str, origin: (usize, usize)) {
    match chosen.iter_mut().find(|(file, _)| file == display) {
        Some((_, origins)) => origins.push(origin),
        None => chosen.push((display.to_string(), vec![origin])),
    }
}

// Asks about each suggestion in turn and returns the chosen origins by file: `y` applies this one,
// `n` skips it, `a` applies this and every remaining one, `q` quits and applies nothing (`None`).
fn choose_interactively(
    out: &mut impl Write,
    input: &mut impl BufRead,
    suggestions: &[Site],
) -> io::Result<Option<Chosen>> {
    let mut chosen = Chosen::new();
    let mut apply_rest = false;
    for (display_path, suggestion) in suggestions {
        if !apply_rest {
            writeln!(out, "{}", format_suggestion(display_path, suggestion))?;
            let answer = ask(out, input, "Apply this? [y/n/a/q] ")?;
            if answer == "q" {
                writeln!(out, "Quit: nothing was changed.")?;
                return Ok(None);
            }
            if answer == "n" {
                writeln!(out)?;
                continue;
            }
            apply_rest = answer == "a";
            writeln!(out)?;
        }
        choose_for(
            &mut chosen,
            display_path,
            (int(suggestion, "line"), int(suggestion, "col")),
        );
    }
    Ok(Some(chosen))
}

type Request = (String, String, Selection);

// What to hand to the apply step for --apply, --apply --only and --interactive; `None` if the
// user quit.
fn edit_requests(
    out: &mut impl Write,
    input: &mut impl BufRead,
    args: &SuggestArgs,
    suggestions: &[Site],
) -> io::Result<Option<Vec<Request>>> {
    if args.apply && args.only.is_empty() {
        let mut safe_files: Vec<&String> = suggestions
            .iter()
            .filter(|(_, s)| text(s, "tier") == "safe")
            .map(|(display, _)| display)
            .collect();
        safe_files.sort();
        safe_files.dedup();
        return Ok(Some(
            safe_files
                .into_iter()
                .map(|display| (display.clone(), display.clone(), Selection::SafeTier))
                .collect(),
        ));
    }
    let chosen = if args.apply {
        // Naming a site is the choice, so its tier doesn't matter, but its notes are shown first.
        let mut chosen = Chosen::new();
        for (display, suggestion) in suggestions {
            for note in suggestion["notes"].as_array().into_iter().flatten() {
                writeln!(
                    out,
                    "{}: note: {}",
                    text(suggestion, "phrase"),
                    note.as_str().unwrap_or("")
                )?;
            }
            choose_for(
                &mut chosen,
                display,
                (int(suggestion, "line"), int(suggestion, "col")),
            );
        }
        chosen
    } else {
        match choose_interactively(out, input, suggestions)? {
            Some(chosen) => chosen,
            None => return Ok(None),
        }
    };
    Ok(Some(
        chosen
            .into_iter()
            .map(|(display, origins)| (display.clone(), display, Selection::Origins(origins)))
            .collect(),
    ))
}

fn report_nothing_to_apply(
    out: &mut impl Write,
    suggested: usize,
    withheld: usize,
    review_hint: bool,
) -> io::Result<()> {
    let hint = if review_hint {
        " Review-tier suggestions are only applied one at a time, with --interactive or --only."
    } else {
        ""
    };
    writeln!(
        out,
        "Nothing to apply ({}, {withheld} withheld).{hint}",
        plural(suggested, "suggestion")
    )
}

// Applies `requests`, then prints what happened; returns how many files couldn't be processed, or
// the exit status when the whole write failed.
fn apply_and_report(
    out: &mut impl Write,
    requests: &[Request],
    index: Option<Arc<ProjectIndex>>,
    require_tracked: bool,
) -> io::Result<Result<usize, i32>> {
    let result: ApplyResult = apply_all(requests, index, require_tracked);
    let (mut applied, mut skipped, mut written, mut failed) = (0, 0, 0, 0);
    for outcome in &result.files {
        if let Some(error) = &outcome.error {
            eprintln!("{}: skipped, {error}", outcome.file);
            failed += 1;
            continue;
        }
        for item in &outcome.applied {
            let columns = item
                .columns
                .iter()
                .map(|c| dumps_str(c))
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(
                out,
                "applied  {}  {}:{}:{}  {}  [{columns}]",
                item.phrase, outcome.file, item.line, item.col, item.var
            )?;
        }
        for item in &outcome.skipped {
            writeln!(
                out,
                "skipped  {}:{}:{}  {}",
                outcome.file, item.line, item.col, item.reason
            )?;
        }
        applied += outcome.applied.len();
        skipped += outcome.skipped.len();
        written += usize::from(outcome.written);
    }
    if let Some(error) = &result.failed {
        eprintln!("Error: {error}");
        return Ok(Err(1));
    }
    let (before, after) = result
        .files
        .iter()
        .filter(|o| o.written)
        .fold((0, 0), |(b, a), o| {
            (b + o.errors_before, a + o.errors_after)
        });
    writeln!(
        out,
        "Applied {} in {}, {skipped} skipped. Re-checked the edited files: {} before, {after} after.",
        plural(applied, "suggestion"),
        plural(written, "file"),
        plural(before, "error")
    )?;
    if written > 0 && require_tracked {
        writeln!(
            out,
            "Review the changes with `git diff`; undo a file with `git restore FILE`."
        )?;
    }
    Ok(Ok(failed))
}

/// Runs `suggest`: prints suggestions, or applies them with `--apply` / `--interactive`.
pub(crate) fn run_suggest(
    args: &SuggestArgs,
    out: &mut impl Write,
    input: &mut impl BufRead,
) -> io::Result<i32> {
    let path = match resolve_existing_path(&args.path) {
        Ok(path) => path,
        Err(code) => return Ok(code),
    };
    if path.extension().is_some_and(|ext| ext == "ipynb") {
        eprintln!("Error: `suggest` doesn't cover notebooks");
        return Ok(2);
    }

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
    let files = collect_files(&path, &[".py"], excludes.as_ref());

    let editing = args.apply || args.interactive;
    if editing && !args.allow_dirty {
        if let Some(problem) = crate::apply::vcs_problem(&root) {
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
