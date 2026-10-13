//! DataFrame schema coverage: the opt-in threshold gate and the `--coverage-detail` reports.
//!
//! Sites and events are the JSON objects the checker produced for each file, tagged with the
//! `file` they came from.

use super::config::{glob_to_regex, pattern_specificity, CoverageConfig};
use super::pathutil::relative_posix;
use super::pyfmt::{repr_str, round_half_even};
use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

const PCT_MAX: f64 = 100.0;

/// One file's DataFrame coverage counts.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) struct Tally {
    pub(crate) total: u64,
    pub(crate) typed: u64,
}

fn pct_of(typed: u64, total: u64) -> f64 {
    PCT_MAX * typed as f64 / total as f64
}

/// One threshold and the aggregate tally it is judged against.
#[derive(Debug, PartialEq)]
pub(crate) struct Bucket {
    /// The override glob this bucket came from, or `None` for the global threshold.
    pub(crate) label: Option<String>,
    pub(crate) threshold: f64,
    pub(crate) total: u64,
    pub(crate) typed: u64,
}

impl Bucket {
    /// Coverage percentage, unrounded so a near miss isn't displayed as a pass.
    fn pct(&self) -> f64 {
        if self.total == 0 {
            PCT_MAX
        } else {
            pct_of(self.typed, self.total)
        }
    }
}

/// The raw per-file lists `--coverage-detail=explain` draws its report from.
#[derive(Default)]
pub(crate) struct Sources<'a> {
    pub(crate) all_dataframe_calls: &'a [Value],
    pub(crate) typed_sites: &'a [Value],
    pub(crate) untyped_sites: &'a [Value],
    pub(crate) leg_events: &'a [Value],
    pub(crate) untyped_site_usage: &'a [Value],
}

// Accessors for the checker's JSON; the checker always writes these keys.
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn int(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(0)
}

// `str(value)` for a JSON scalar, as Python would print it.
fn py_str(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "None".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(true)) => "True".to_string(),
        Some(Value::Bool(false)) => "False".to_string(),
        Some(other) => other.to_string(),
    }
}

fn by_file(sites: &[Value]) -> BTreeMap<&str, Vec<&Value>> {
    let mut grouped: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for site in sites {
        grouped.entry(text(site, "file")).or_default().push(site);
    }
    grouped
}

/// A site's position: `line:col`, or `cell N:line:col` for a notebook site.
pub(crate) fn location(site: &Value) -> String {
    let prefix = site
        .get("cell")
        .map(|cell| format!("cell {}:", py_str(Some(cell))))
        .unwrap_or_default();
    format!("{prefix}{}:{}", int(site, "line"), int(site, "col"))
}

fn missing_label(site: &Value) -> String {
    let prefix = site
        .get("cell")
        .map(|cell| format!("cell {}:", py_str(Some(cell))))
        .unwrap_or_default();
    format!("{prefix}{}:{}", text(site, "var"), int(site, "line"))
}

fn by_line(sites: &[&Value]) -> Vec<Value> {
    let mut sorted: Vec<&Value> = sites.to_vec();
    sorted.sort_by_key(|site| int(site, "line"));
    sorted.into_iter().cloned().collect()
}

fn site_labels(sites: &[&Value]) -> Vec<String> {
    by_line(sites).iter().map(missing_label).collect()
}

// Reading order: notebook cell first, then line, then column.
fn position_key(site: &&Value) -> (i64, i64, i64) {
    (int(site, "cell"), int(site, "line"), int(site, "col"))
}

fn sorted_by_position<'a>(sites: Option<&Vec<&'a Value>>) -> Vec<&'a Value> {
    let mut sorted: Vec<&Value> = sites.cloned().unwrap_or_default();
    sorted.sort_by_key(position_key);
    sorted
}

/// Groups checked files by the threshold that governs them and returns the failures.
///
/// Each file lands in exactly one bucket (the most specific matching override, else the global
/// `fail_under`) and each bucket is graded on its own aggregate ratio. `--coverage-fail-under` is
/// a total override: one threshold for every file, per-path overrides ignored. A bucket with no
/// recognised DataFrames passes vacuously.
pub(crate) fn evaluate_coverage(
    per_file: &BTreeMap<String, Tally>,
    config: &CoverageConfig,
    root: &Path,
    cli_fail_under: Option<f64>,
) -> Vec<Bucket> {
    let mut regexes: HashMap<&str, Regex> = HashMap::new();
    let mut tallies: Vec<(Option<String>, f64, Tally)> = Vec::new();
    for (file, tally) in per_file {
        let (label, threshold) = match cli_fail_under {
            Some(threshold) => (None, threshold),
            None => {
                let relative = relative_posix(file, root);
                let best = config
                    .overrides
                    .iter()
                    .filter(|(pattern, _)| {
                        regexes
                            .entry(pattern.as_str())
                            .or_insert_with(|| glob_to_regex(pattern))
                            .is_match(&relative)
                    })
                    .max_by(|a, b| pattern_specificity(&a.0).cmp(&pattern_specificity(&b.0)));
                match best {
                    Some((pattern, threshold)) => (Some(pattern.clone()), *threshold),
                    None => (None, config.fail_under),
                }
            }
        };
        match tallies.iter_mut().find(|(known, _, _)| *known == label) {
            Some((_, stored, sum)) => {
                *stored = threshold;
                sum.total += tally.total;
                sum.typed += tally.typed;
            }
            None => tallies.push((label, threshold, *tally)),
        }
    }

    let mut failing: Vec<Bucket> = tallies
        .into_iter()
        .filter(|(_, _, tally)| tally.total != 0)
        .map(|(label, threshold, tally)| Bucket {
            label,
            threshold,
            total: tally.total,
            typed: tally.typed,
        })
        .filter(|bucket| bucket.pct() < bucket.threshold)
        .collect();
    // The global bucket first, then overrides alphabetically, so output is stable.
    failing.sort_by(|a, b| {
        (a.label.is_some(), a.label.as_deref().unwrap_or(""))
            .cmp(&(b.label.is_some(), b.label.as_deref().unwrap_or("")))
    });
    failing
}

/// Explains one failed threshold: what was required, what was measured, and where. The percentage
/// has one decimal, so 99.6% against a gate of 100 reads as a failure.
pub(crate) fn failure_message(bucket: &Bucket) -> String {
    let scope = bucket
        .label
        .as_ref()
        .map(|label| format!(" for {}", repr_str(label)))
        .unwrap_or_default();
    format!(
        "\u{2717} DataFrame schema coverage {:.1}% is below the required {:.1}%{scope} ({}/{} DataFrames had column info)",
        bucket.pct(),
        bucket.threshold,
        bucket.typed,
        bucket.total
    )
}

/// The per-file coverage table plus the sites that cost coverage, modelled on `coverage report -m`.
/// Files with no recognised DataFrames are left out.
pub(crate) fn format_term_missing(
    per_file: &BTreeMap<String, Tally>,
    untyped_sites: &[Value],
    root: &Path,
) -> String {
    let rows: Vec<(&String, &Tally)> = per_file.iter().filter(|(_, t)| t.total > 0).collect();
    if rows.is_empty() {
        return "No DataFrames with recognized loads/schemas found to check".to_string();
    }
    let missing = by_file(untyped_sites);
    let display: Vec<String> = rows
        .iter()
        .map(|(name, _)| relative_posix(name, root))
        .collect();
    let name_width = display
        .iter()
        .map(|d| d.chars().count())
        .max()
        .unwrap_or(0)
        .max("Name".len());
    let header = format!("{:<name_width$}  Typed  Total   Cover   Missing", "Name");
    let rule = "-".repeat(header.chars().count());
    let mut lines = vec![header, rule.clone()];

    for ((name, tally), shown) in rows.iter().zip(&display) {
        let pct = round_half_even(PCT_MAX * tally.typed as f64 / tally.total as f64);
        let labels = site_labels(missing.get(name.as_str()).map_or(&[][..], Vec::as_slice));
        let row = format!(
            "{shown:<name_width$}  {:>5}  {:>5}  {pct:>5}%   {}",
            tally.typed,
            tally.total,
            labels.join(", ")
        );
        lines.push(row.trim_end().to_string());
    }

    let total_all: u64 = rows.iter().map(|(_, t)| t.total).sum();
    let typed_all: u64 = rows.iter().map(|(_, t)| t.typed).sum();
    let pct_all = round_half_even(PCT_MAX * typed_all as f64 / total_all as f64);
    lines.push(rule);
    lines.push(format!(
        "{:<name_width$}  {typed_all:>5}  {total_all:>5}  {pct_all:>5}%",
        "TOTAL"
    ));
    lines.join("\n")
}

fn site_entries(sites: &[&Value]) -> Vec<Value> {
    by_line(sites)
        .iter()
        .map(|site| {
            let mut entry = Map::new();
            entry.insert("var".into(), site["var"].clone());
            entry.insert("line".into(), site["line"].clone());
            entry.insert("col".into(), site["col"].clone());
            if let Some(cell) = site.get("cell") {
                entry.insert("cell".into(), cell.clone());
            }
            Value::Object(entry)
        })
        .collect()
}

/// The machine-readable coverage document nested under `--output-format=json`'s `coverage` key.
/// Percentages are unrounded: a consumer deciding whether a gate passed needs the real ratio.
pub(crate) fn coverage_json(
    per_file: &BTreeMap<String, Tally>,
    untyped_sites: &[Value],
    root: &Path,
) -> Value {
    let missing = by_file(untyped_sites);
    let percent = |typed: u64, total: u64| {
        if total == 0 {
            Value::Null
        } else {
            json!(pct_of(typed, total))
        }
    };
    let files: Vec<Value> = per_file
        .iter()
        .map(|(name, tally)| {
            json!({
                "file": relative_posix(name, root),
                "dataframes_total": tally.total,
                "dataframes_typed": tally.typed,
                "percent": percent(tally.typed, tally.total),
                "missing": site_entries(missing.get(name.as_str()).map_or(&[][..], Vec::as_slice)),
            })
        })
        .collect();
    let total_all: u64 = per_file.values().map(|t| t.total).sum();
    let typed_all: u64 = per_file.values().map(|t| t.typed).sum();
    json!({
        "dataframes_total": total_all,
        "dataframes_typed": typed_all,
        "percent": percent(typed_all, total_all),
        "files": files,
    })
}

// Classifies one `all_dataframe_calls` site against the typed and untyped sites, by line. By line
// and not by column: the sites record the enclosing statement's position and the call its own,
// which differ on the same line by design. Two DataFrame-shaped calls on one line are ambiguous
// under this scheme, an accepted and documented limitation.
fn explain_status(
    site: &Value,
    typed_by_line: &HashMap<i64, &Value>,
    untyped_lines: &HashSet<i64>,
) -> String {
    let line = int(site, "line");
    if let Some(typed) = typed_by_line.get(&line) {
        return format!("COUNTED, typed (schema: {})", py_str(typed.get("schema")));
    }
    if untyped_lines.contains(&line) {
        return "COUNTED, untracked -- no static column info".to_string();
    }
    format!("NOT COUNTED \u{2014} {}", text(site, "context"))
}

fn consumption_tag(consumes: &str) -> &'static str {
    match consumes {
        "all" => " [touches every column]",
        "none" => " [touches no columns]",
        _ => "",
    }
}

// Identical lines merged into the first occurrence with a `(xN)` count.
fn collapse_repeats(lines: Vec<String>) -> Vec<String> {
    let mut order: Vec<String> = Vec::new();
    let mut counts: HashMap<String, usize> = HashMap::new();
    for line in lines {
        let count = counts.entry(line.clone()).or_insert(0);
        if *count == 0 {
            order.push(line);
        }
        *count += 1;
    }
    order
        .into_iter()
        .map(|line| match counts[&line] {
            1 => line,
            n => format!("{line} (x{n})"),
        })
        .collect()
}

// One text line per observed access or other use of an untyped site's frame, each flagged
// "(conditional)" when it doesn't happen on every execution path.
fn usage_lines(usage: &Value) -> Vec<String> {
    let place = format!("{:<12} {:<16} ", location(usage), text(usage, "var"));
    let conditional = |item: &Value| {
        if item
            .get("conditional")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            " (conditional)"
        } else {
            ""
        }
    };
    let mut lines = Vec::new();
    for access in usage["accesses"].as_array().into_iter().flatten() {
        lines.push(format!(
            "{place}'{}'{}",
            text(access, "column"),
            conditional(access)
        ));
    }
    for other in usage["other_uses"].as_array().into_iter().flatten() {
        lines.push(format!(
            "{place}{}{}{}",
            text(other, "description"),
            conditional(other),
            consumption_tag(text(other, "consumes"))
        ));
    }
    collapse_repeats(lines)
}

fn files_in(sources: &Sources) -> BTreeSet<String> {
    [
        sources.all_dataframe_calls,
        sources.leg_events,
        sources.untyped_site_usage,
    ]
    .iter()
    .flat_map(|sites| sites.iter().map(|site| text(site, "file").to_string()))
    .collect()
}

/// Every DataFrame-shaped call site found in each file, counted or not, then the statements after
/// which a tracked frame's columns stopped being known, then the columns an untyped site's frame
/// was seen to use downstream.
pub(crate) fn format_explain(sources: &Sources, root: &Path) -> String {
    if sources.all_dataframe_calls.is_empty()
        && sources.leg_events.is_empty()
        && sources.untyped_site_usage.is_empty()
    {
        return "No DataFrame-shaped calls found to check".to_string();
    }
    let calls = by_file(sources.all_dataframe_calls);
    let typed = by_file(sources.typed_sites);
    let untyped = by_file(sources.untyped_sites);
    let events = by_file(sources.leg_events);
    let usage = by_file(sources.untyped_site_usage);

    let mut lines: Vec<String> = Vec::new();
    for name in files_in(sources) {
        let name = name.as_str();
        let typed_by_line: HashMap<i64, &Value> = typed
            .get(name)
            .into_iter()
            .flatten()
            .map(|site| (int(site, "line"), *site))
            .collect();
        let untyped_here: &[&Value] = untyped.get(name).map_or(&[], Vec::as_slice);
        let untyped_lines: HashSet<i64> = untyped_here.iter().map(|s| int(s, "line")).collect();
        let mut sites_per_line: HashMap<i64, usize> = HashMap::new();
        for site in untyped_here {
            *sites_per_line.entry(int(site, "line")).or_default() += 1;
        }
        let phrase_by_line: HashMap<i64, &str> = untyped_here
            .iter()
            .filter(|s| !text(s, "phrase").is_empty() && sites_per_line[&int(s, "line")] == 1)
            .map(|s| (int(s, "line"), text(s, "phrase")))
            .collect();

        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(relative_posix(name, root));
        for site in sorted_by_position(calls.get(name)) {
            let mut status = explain_status(site, &typed_by_line, &untyped_lines);
            if let Some(phrase) = phrase_by_line.get(&int(site, "line")) {
                if status.starts_with("COUNTED, untracked") {
                    status = format!("{status}  [{phrase}]");
                }
            }
            lines.push(format!(
                "  {:<12} {:<16} {status}",
                location(site),
                text(site, "label")
            ));
        }
        let file_events = sorted_by_position(events.get(name));
        if !file_events.is_empty() {
            lines.push("  tracking ended:".to_string());
            for event in file_events {
                lines.push(format!(
                    "    {:<12} {:<16} {} -- {}",
                    location(event),
                    text(event, "var"),
                    text(event, "outcome"),
                    text(event, "reason")
                ));
            }
        }
        let file_usage = sorted_by_position(usage.get(name));
        if !file_usage.is_empty() {
            lines.push("  columns used downstream:".to_string());
            for item in file_usage {
                lines.extend(
                    usage_lines(item)
                        .into_iter()
                        .map(|line| format!("    {line}")),
                );
            }
        }
    }
    lines.join("\n")
}

// An object with `line` and `col`, then the notebook `cell` if there is one, then `rest`.
fn positioned(site: &Value, rest: Vec<(&str, Value)>) -> Value {
    let mut entry = Map::new();
    entry.insert("line".into(), site["line"].clone());
    entry.insert("col".into(), site["col"].clone());
    if let Some(cell) = site.get("cell") {
        entry.insert("cell".into(), cell.clone());
    }
    for (key, value) in rest {
        entry.insert(key.into(), value);
    }
    Value::Object(entry)
}

/// The machine-readable `--coverage-detail=explain` document: the structured counterpart to
/// [`format_explain`]. A file with tracking-ended events but no calls still appears, with an empty
/// `calls`.
pub(crate) fn explain_json(sources: &Sources, root: &Path) -> Value {
    let calls = by_file(sources.all_dataframe_calls);
    let typed = by_file(sources.typed_sites);
    let untyped = by_file(sources.untyped_sites);
    let events = by_file(sources.leg_events);
    let usage = by_file(sources.untyped_site_usage);

    let mut files = Vec::new();
    for name in files_in(sources) {
        let name = name.as_str();
        let typed_by_line: HashMap<i64, &Value> = typed
            .get(name)
            .into_iter()
            .flatten()
            .map(|site| (int(site, "line"), *site))
            .collect();
        let untyped_lines: HashSet<i64> = untyped
            .get(name)
            .into_iter()
            .flatten()
            .map(|s| int(s, "line"))
            .collect();
        let call_entries: Vec<Value> = sorted_by_position(calls.get(name))
            .into_iter()
            .map(|site| {
                let line = int(site, "line");
                let (counted, is_typed, schema) = if let Some(found) = typed_by_line.get(&line) {
                    (true, true, found["schema"].clone())
                } else if untyped_lines.contains(&line) {
                    (true, false, Value::Null)
                } else {
                    (false, false, Value::Null)
                };
                positioned(
                    site,
                    vec![
                        ("label", site["label"].clone()),
                        ("counted", json!(counted)),
                        ("typed", json!(is_typed)),
                        ("schema", schema),
                        ("context", site["context"].clone()),
                    ],
                )
            })
            .collect();
        let ended: Vec<Value> = sorted_by_position(events.get(name))
            .into_iter()
            .map(|event| {
                positioned(
                    event,
                    vec![
                        ("var", event["var"].clone()),
                        ("outcome", event["outcome"].clone()),
                        ("reason", event["reason"].clone()),
                    ],
                )
            })
            .collect();
        let used: Vec<Value> = sorted_by_position(usage.get(name))
            .into_iter()
            .map(|item| {
                positioned(
                    item,
                    vec![
                        ("var", item["var"].clone()),
                        ("accesses", item["accesses"].clone()),
                        ("other_uses", item["other_uses"].clone()),
                    ],
                )
            })
            .collect();
        files.push(json!({
            "file": relative_posix(name, root),
            "calls": call_entries,
            "tracking_ended": ended,
            "columns_used": used,
        }));
    }
    json!({ "files": files })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tallies(entries: &[(&str, u64, u64)]) -> BTreeMap<String, Tally> {
        entries
            .iter()
            .map(|(name, typed, total)| {
                (
                    name.to_string(),
                    Tally {
                        total: *total,
                        typed: *typed,
                    },
                )
            })
            .collect()
    }

    fn config(fail_under: f64, overrides: &[(&str, f64)]) -> CoverageConfig {
        CoverageConfig {
            enabled: true,
            fail_under,
            overrides: overrides.iter().map(|(p, t)| (p.to_string(), *t)).collect(),
            ..CoverageConfig::default()
        }
    }

    #[test]
    fn test_should_fail_the_global_bucket_below_its_threshold() {
        // arrange
        let per_file = tallies(&[("/p/a.py", 1, 4), ("/p/b.py", 2, 4)]);

        // act
        let failing = evaluate_coverage(&per_file, &config(50.0, &[]), Path::new("/p"), None);

        // assert
        assert_eq!(1, failing.len());
        assert_eq!(
            (3, 8, None),
            (failing[0].typed, failing[0].total, failing[0].label.clone())
        );
    }

    #[test]
    fn test_should_pass_a_bucket_with_no_dataframes() {
        // arrange
        let per_file = tallies(&[("/p/a.py", 0, 0)]);

        // act
        let failing = evaluate_coverage(&per_file, &config(100.0, &[]), Path::new("/p"), None);

        // assert
        assert!(failing.is_empty());
    }

    #[test]
    fn test_should_grade_a_legacy_subtree_on_its_own_override() {
        // arrange
        let per_file = tallies(&[("/p/legacy/a.py", 1, 4), ("/p/new/b.py", 4, 4)]);
        let settings = config(90.0, &[("legacy/**", 20.0)]);

        // act
        let failing = evaluate_coverage(&per_file, &settings, Path::new("/p"), None);

        // assert
        assert!(failing.is_empty());
    }

    #[test]
    fn test_should_let_the_command_line_threshold_ignore_overrides() {
        // arrange
        let per_file = tallies(&[("/p/legacy/a.py", 1, 4)]);
        let settings = config(90.0, &[("legacy/**", 20.0)]);

        // act
        let failing = evaluate_coverage(&per_file, &settings, Path::new("/p"), Some(100.0));

        // assert
        assert_eq!(1, failing.len());
        assert_eq!(None, failing[0].label);
    }

    #[test]
    fn test_should_pick_the_most_specific_matching_override() {
        // arrange
        let per_file = tallies(&[("/p/src/new_module/a.py", 1, 2)]);
        let settings = config(100.0, &[("src/**", 10.0), ("src/new_module/**", 90.0)]);

        // act
        let failing = evaluate_coverage(&per_file, &settings, Path::new("/p"), None);

        // assert
        assert_eq!(Some("src/new_module/**".to_string()), failing[0].label);
    }

    #[test]
    fn test_should_put_the_global_failure_before_override_failures() {
        // arrange
        let per_file = tallies(&[("/p/b/x.py", 0, 2), ("/p/a/y.py", 0, 2), ("/p/z.py", 0, 2)]);
        let settings = config(50.0, &[("b/**", 50.0), ("a/**", 50.0)]);

        // act
        let failing = evaluate_coverage(&per_file, &settings, Path::new("/p"), None);

        // assert
        let labels: Vec<Option<String>> = failing.iter().map(|b| b.label.clone()).collect();
        assert_eq!(vec![None, Some("a/**".into()), Some("b/**".into())], labels);
    }

    #[test]
    fn test_should_show_one_decimal_and_the_scope_in_a_failure() {
        // arrange
        let bucket = Bucket {
            label: Some("legacy/**".into()),
            threshold: 100.0,
            total: 1000,
            typed: 996,
        };

        // act
        let message = failure_message(&bucket);

        // assert
        assert_eq!(
            "\u{2717} DataFrame schema coverage 99.6% is below the required 100.0% for 'legacy/**' (996/1000 DataFrames had column info)",
            message
        );
    }

    #[test]
    fn test_should_list_missing_sites_in_a_term_missing_table() {
        // arrange
        let per_file = tallies(&[("/p/a.py", 1, 3), ("/p/empty.py", 0, 0)]);
        let sites = vec![
            json!({"file": "/p/a.py", "var": "df2", "line": 9, "col": 1}),
            json!({"file": "/p/a.py", "var": "df1", "line": 3, "col": 1}),
        ];

        // act
        let table = format_term_missing(&per_file, &sites, Path::new("/p"));

        // assert
        let expected = "\
Name  Typed  Total   Cover   Missing
------------------------------------
a.py      1      3     33%   df1:3, df2:9
------------------------------------
TOTAL      1      3     33%";
        assert_eq!(expected, table);
    }

    #[test]
    fn test_should_say_so_when_no_file_has_dataframes() {
        // arrange
        let per_file = tallies(&[("/p/a.py", 0, 0)]);

        // act
        let table = format_term_missing(&per_file, &[], Path::new("/p"));

        // assert
        assert_eq!(
            "No DataFrames with recognized loads/schemas found to check",
            table
        );
    }

    #[test]
    fn test_should_leave_percent_null_for_a_file_without_dataframes() {
        // arrange
        let per_file = tallies(&[("/p/a.py", 0, 0)]);

        // act
        let payload = coverage_json(&per_file, &[], Path::new("/p"));

        // assert
        assert_eq!(Value::Null, payload["percent"]);
        assert_eq!(Value::Null, payload["files"][0]["percent"]);
        assert_eq!("a.py", payload["files"][0]["file"]);
    }

    fn explain_sources_fixture() -> (Vec<Value>, Vec<Value>, Vec<Value>) {
        let calls = vec![
            json!({"file": "/p/a.py", "line": 2, "col": 5, "label": "pd.read_csv", "context": "assignment", "phrase": null}),
            json!({"file": "/p/a.py", "line": 7, "col": 5, "label": "pd.read_csv", "context": "a bare return"}),
            json!({"file": "/p/a.py", "line": 9, "col": 5, "label": "pd.read_json", "context": "x"}),
        ];
        let typed = vec![json!({"file": "/p/a.py", "line": 2, "schema": "Orders"})];
        let untyped = vec![
            json!({"file": "/p/a.py", "line": 9, "col": 1, "var": "df", "phrase": "quiet-amber"}),
        ];
        (calls, typed, untyped)
    }

    #[test]
    fn test_should_classify_each_call_in_the_explain_text() {
        // arrange
        let (calls, typed, untyped) = explain_sources_fixture();
        let sources = Sources {
            all_dataframe_calls: &calls,
            typed_sites: &typed,
            untyped_sites: &untyped,
            ..Sources::default()
        };

        // act
        let report = format_explain(&sources, Path::new("/p"));

        // assert
        let expected = "\
a.py
  2:5          pd.read_csv      COUNTED, typed (schema: Orders)
  7:5          pd.read_csv      NOT COUNTED \u{2014} a bare return
  9:5          pd.read_json     COUNTED, untracked -- no static column info  [quiet-amber]";
        assert_eq!(expected, report);
    }

    #[test]
    fn test_should_mirror_the_explain_text_in_the_explain_json() {
        // arrange
        let (calls, typed, untyped) = explain_sources_fixture();
        let sources = Sources {
            all_dataframe_calls: &calls,
            typed_sites: &typed,
            untyped_sites: &untyped,
            ..Sources::default()
        };

        // act
        let payload = explain_json(&sources, Path::new("/p"));

        // assert
        let entries = &payload["files"][0]["calls"];
        assert_eq!(json!(true), entries[0]["typed"]);
        assert_eq!(json!("Orders"), entries[0]["schema"]);
        assert_eq!(json!(false), entries[1]["counted"]);
        assert_eq!(json!(true), entries[2]["counted"]);
        let keys: Vec<&String> = entries[0].as_object().unwrap().keys().collect();
        assert_eq!(
            vec!["line", "col", "label", "counted", "typed", "schema", "context"],
            keys
        );
    }

    #[test]
    fn test_should_collapse_repeated_usage_lines_with_a_count() {
        // arrange
        let usage = json!({
            "file": "/p/a.py", "line": 4, "col": 1, "var": "df",
            "accesses": [{"column": "a", "conditional": false}, {"column": "a", "conditional": false}],
            "other_uses": [{"description": "passed to f()", "conditional": true, "consumes": "all"}],
        });

        // act
        let lines = usage_lines(&usage);

        // assert
        assert_eq!(
            vec![
                "4:1          df               'a' (x2)".to_string(),
                "4:1          df               passed to f() (conditional) [touches every column]"
                    .to_string(),
            ],
            lines
        );
    }

    #[test]
    fn test_should_report_no_calls_found_when_there_is_nothing_to_explain() {
        assert_eq!(
            "No DataFrame-shaped calls found to check",
            format_explain(&Sources::default(), &PathBuf::from("/p"))
        );
    }
}
