//! `typedframes report`: bug-report bodies for untyped sites.

use super::args::{Outcome, ReportArgs, ReportFormat};
use super::check::{build_index, resolve_existing_path};
use super::collect::collect_files;
use super::config::load_configured_excludes;
use super::pyfmt::{dumps_indent2, repr_str};
use crate::engine::{report_all_batch, report_batch};
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const REVIEW_NOTICE: &str = "<!-- Review before posting, then delete this line. Names, comments and most strings are replaced, but numbers 0 and 1, library and option names and the shape of the code are kept as written. -->";

const REVIEW_REMINDER: &str = "Read the report before posting it: names, comments and most strings are replaced, but the shape of the code and library or option names are kept.";

// What the report says about the tool and where it ran.
fn environment_line() -> String {
    format!(
        "- typedframes {} on {}/{}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

// How many more uses a reason mentions; sites differing only in that are one problem.
fn strip_more_uses(reason: &str) -> String {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN
        .get_or_init(|| Regex::new(", and N more$").expect("a fixed pattern compiles"))
        .replace(reason, "")
        .into_owned()
}

fn snippet(report: &Value) -> Option<&str> {
    report.get("snippet").and_then(Value::as_str)
}

/// The Markdown issue body for one site's report.
pub(crate) fn format_report(report: &Value) -> String {
    let verdict = &report["verdict"];
    let mut outcome = text(verdict, "outcome").to_string();
    if let Some(kind) = verdict
        .get("kind")
        .and_then(Value::as_str)
        .filter(|k| !k.is_empty())
    {
        outcome = format!("{outcome} (`{kind}`, {})", text(verdict, "tier"));
    }
    let mut lines = vec![
        REVIEW_NOTICE.to_string(),
        format!("## typedframes report: {}", text(report, "name")),
        String::new(),
        environment_line(),
        String::new(),
        "### What the checker did".to_string(),
        String::new(),
        format!("- Outcome: {outcome}"),
        format!("- Reason: {}", text(verdict, "reason")),
    ];
    for note in verdict["notes"].as_array().into_iter().flatten() {
        lines.push(format!("- Note: {}", note.as_str().unwrap_or("")));
    }
    lines.extend(["".to_string(), "### Code".to_string(), "".to_string()]);
    match snippet(report).filter(|s| !s.is_empty()) {
        Some(code) => {
            lines.push("```python".to_string());
            lines.push(code.trim_end_matches('\n').to_string());
            lines.push("```".to_string());
        }
        None => lines.push(format!("No snippet: {}.", text(report, "refusal"))),
    }
    lines.extend([
        String::new(),
        "### What you expected".to_string(),
        String::new(),
        "<!-- Describe what you expected instead. -->".to_string(),
    ]);
    lines.join("\n")
}

type Group = Vec<Value>;

fn null_as_text(value: &Value) -> String {
    match value {
        Value::Null => "\0".to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Groups site reports by what the checker decided and why, largest group first.
fn group_reports(reports: Vec<Value>) -> Vec<Group> {
    let mut index: HashMap<[String; 4], usize> = HashMap::new();
    let mut groups: Vec<Group> = Vec::new();
    for report in reports {
        let verdict = &report["verdict"];
        let key = [
            null_as_text(&verdict["outcome"]),
            null_as_text(&verdict["kind"]),
            null_as_text(&verdict["tier"]),
            strip_more_uses(text(verdict, "reason")),
        ];
        match index.get(&key) {
            Some(&at) => groups[at].push(report),
            None => {
                index.insert(key, groups.len());
                groups.push(vec![report]);
            }
        }
    }
    groups.sort_by(|a, b| {
        b.len()
            .cmp(&a.len())
            .then_with(|| text(&a[0], "name").cmp(text(&b[0], "name")))
    });
    groups
}

// The report in a group that is easiest to read: one with a snippet, the shortest.
fn representative(group: &Group) -> &Value {
    group
        .iter()
        .min_by_key(|r| {
            let code = snippet(r);
            (code.is_none(), code.map_or(0, |c| c.chars().count()))
        })
        .expect("a group has at least one report")
}

fn group_title(group: &Group) -> String {
    let verdict = &group[0]["verdict"];
    format!(
        "{}: {}",
        text(verdict, "outcome"),
        strip_more_uses(text(verdict, "reason"))
    )
}

fn names(group: &Group) -> Vec<&str> {
    group.iter().map(|r| text(r, "name")).collect()
}

fn format_report_groups(groups: &[Group]) -> String {
    let total: usize = groups.iter().map(Vec::len).sum();
    let sites = format!(
        "{total} untyped {}",
        if total == 1 { "site" } else { "sites" }
    );
    let group_count = format!(
        "{} {}",
        groups.len(),
        if groups.len() == 1 { "group" } else { "groups" }
    );
    let mut lines = vec![
        REVIEW_NOTICE.to_string(),
        format!("## typedframes report: {sites} in {group_count}"),
        String::new(),
        environment_line(),
        String::new(),
        "| Sites | What the checker did |".to_string(),
        "| ---: | --- |".to_string(),
    ];
    for group in groups {
        lines.push(format!("| {} | {} |", group.len(), group_title(group)));
    }
    for (number, group) in groups.iter().enumerate() {
        let best = representative(group);
        lines.extend([
            String::new(),
            format!("### {}. {}", number + 1, group_title(group)),
            String::new(),
            format!("Sites: {}", names(group).join(", ")),
            String::new(),
        ]);
        match snippet(best).filter(|s| !s.is_empty()) {
            Some(code) => {
                lines.push(format!("Smallest example, `{}`:", text(best, "name")));
                lines.push(String::new());
                lines.push("```python".to_string());
                lines.push(code.trim_end_matches('\n').to_string());
                lines.push("```".to_string());
            }
            None => lines.push(format!("No snippet: {}.", text(best, "refusal"))),
        }
    }
    lines.join("\n")
}

// A few lines per group, so a whole report fits in one screenshot.
fn format_report_groups_compact(groups: &[Group]) -> String {
    let total: usize = groups.iter().map(Vec::len).sum();
    let mut lines = vec![format!(
        "typedframes {} {}/{}: {total} {}, {} {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        if total == 1 { "site" } else { "sites" },
        groups.len(),
        if groups.len() == 1 { "group" } else { "groups" }
    )];
    for (number, group) in groups.iter().enumerate() {
        let best = representative(group);
        lines.push(format!(
            "{}. x{} {}",
            number + 1,
            group.len(),
            group_title(group)
        ));
        match snippet(best).filter(|s| !s.is_empty()) {
            Some(code) => lines.extend(
                code.lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(|l| format!("    {l}")),
            ),
            None => lines.push(format!("    no snippet: {}", text(best, "refusal"))),
        }
    }
    lines.join("\n")
}

fn report_groups_json(groups: &[Group]) -> String {
    let entries: Vec<Value> = groups
        .iter()
        .map(|group| {
            let verdict = &group[0]["verdict"];
            let best = representative(group);
            json!({
                "count": group.len(),
                "outcome": verdict["outcome"],
                "kind": verdict["kind"],
                "tier": verdict["tier"],
                "reason": strip_more_uses(text(verdict, "reason")),
                "names": names(group),
                "snippet": best["snippet"],
                "refusal": best["refusal"],
            })
        })
        .collect();
    let payload = json!({
        "typedframes": env!("CARGO_PKG_VERSION"),
        "sites": groups.iter().map(Vec::len).sum::<usize>(),
        "groups": entries,
    });
    dumps_indent2(&payload)
}

// Writes one bug-report body per group; returns the files written.
fn write_report_groups(groups: &[Group], directory: &Path) -> io::Result<Vec<PathBuf>> {
    fs::create_dir_all(directory)?;
    let mut written = Vec::new();
    for (number, group) in groups.iter().enumerate() {
        let best = representative(group);
        let mut body = format_report(best);
        let others: Vec<&str> = group
            .iter()
            .filter(|r| !std::ptr::eq(*r, best))
            .map(|r| text(r, "name"))
            .collect();
        if !others.is_empty() {
            body.push_str(&format!(
                "\n\nThe checker decided the same way at {} other sites: {}.",
                others.len(),
                others.join(", ")
            ));
        }
        let file = directory.join(format!("{:02}-{}.md", number + 1, text(best, "name")));
        fs::write(&file, format!("{body}\n"))?;
        written.push(file);
    }
    Ok(written)
}

// The site name (None with --all) and path a `report` call names; exit status 2 on a bad
// combination.
fn report_target(args: &ReportArgs) -> Result<(Option<String>, PathBuf), i32> {
    if args.all {
        if args.name.is_some() && args.path != Path::new(".") {
            eprintln!("Error: `report --all` takes a path, not a site name");
            return Err(2);
        }
        let path = args
            .name
            .as_ref()
            .map_or_else(|| args.path.clone(), PathBuf::from);
        return Ok((None, path));
    }
    match &args.name {
        None => {
            eprintln!("Error: `report` needs a site name, or --all");
            Err(2)
        }
        Some(name) => Ok((Some(name.clone()), args.path.clone())),
    }
}

fn run_report_all(args: &ReportArgs, path: &Path, out: &mut impl Write) -> io::Result<i32> {
    let index = build_index(
        path,
        args.no_index,
        args.cache.cache_dir.as_deref(),
        args.cache.no_cache,
        true,
    );
    let excludes = load_configured_excludes(path);
    let files = collect_files(path, &[".py"], excludes.as_ref());
    let names: Vec<String> = files
        .iter()
        .map(|f| f.to_string_lossy().into_owned())
        .collect();
    let mut reports: Vec<Value> = Vec::new();
    for (name, (result, error)) in names.iter().zip(report_all_batch(&names, index)) {
        let Some(found) = result
            .as_deref()
            .and_then(|j| serde_json::from_str::<Value>(j).ok())
        else {
            eprintln!("{name}: skipped, {}", error.as_deref().unwrap_or("None"));
            continue;
        };
        reports.extend(found.as_array().cloned().unwrap_or_default());
    }
    if let Some(outcome) = args.outcome {
        let wanted = match outcome {
            Outcome::Suggested => "suggested",
            Outcome::Withheld => "withheld",
        };
        reports.retain(|r| text(&r["verdict"], "outcome") == wanted);
    }
    if reports.is_empty() {
        writeln!(out, "No untyped sites.")?;
        return Ok(0);
    }
    let groups = group_reports(reports);
    if let Some(directory) = &args.output_dir {
        for file in write_report_groups(&groups, directory)? {
            writeln!(out, "{}", file.display())?;
        }
    } else if args.report_format == ReportFormat::Json {
        writeln!(out, "{}", report_groups_json(&groups))?;
    } else if args.report_format == ReportFormat::Compact {
        writeln!(out, "{}", format_report_groups_compact(&groups))?;
    } else {
        writeln!(out, "{}", format_report_groups(&groups))?;
    }
    eprintln!("{REVIEW_REMINDER}");
    Ok(0)
}

/// Runs `report`: one site's bug-report body, or every site's with `--all`.
pub(crate) fn run_report(args: &ReportArgs, out: &mut impl Write) -> io::Result<i32> {
    let (name, target) = match report_target(args) {
        Ok(found) => found,
        Err(code) => return Ok(code),
    };
    let path = match resolve_existing_path(&target) {
        Ok(path) => path,
        Err(code) => return Ok(code),
    };
    if path.extension().is_some_and(|ext| ext == "ipynb") {
        eprintln!("Error: `report` doesn't cover notebooks");
        return Ok(2);
    }
    let Some(name) = name else {
        return run_report_all(args, &path, out);
    };

    let index = build_index(
        &path,
        args.no_index,
        args.cache.cache_dir.as_deref(),
        args.cache.no_cache,
        true,
    );
    let excludes = load_configured_excludes(&path);
    let files = collect_files(&path, &[".py"], excludes.as_ref());
    let names: Vec<String> = files
        .iter()
        .map(|f| f.to_string_lossy().into_owned())
        .collect();
    let wanted = name.trim().to_lowercase();
    let mut matches: Vec<(String, Value)> = Vec::new();
    for (file, (result, error)) in names.iter().zip(report_batch(&names, &wanted, index)) {
        let Some(report) = result
            .as_deref()
            .and_then(|j| serde_json::from_str::<Value>(j).ok())
        else {
            eprintln!("{file}: skipped, {}", error.as_deref().unwrap_or("None"));
            continue;
        };
        if !report.is_null() {
            matches.push((file.clone(), report));
        }
    }
    if matches.is_empty() {
        eprintln!("Error: no untyped site is named {}", repr_str(&name));
        return Ok(2);
    }
    if matches.len() > 1 {
        eprintln!(
            "Error: {} names {} sites; narrow the path to one of them:",
            repr_str(&name),
            matches.len()
        );
        for (file, report) in &matches {
            eprintln!(
                "  {file}:{}:{}  {}",
                report["line"],
                report["col"],
                text(report, "var")
            );
        }
        return Ok(2);
    }
    writeln!(out, "{}", format_report(&matches[0].1))?;
    eprintln!("{REVIEW_REMINDER}");
    Ok(0)
}
