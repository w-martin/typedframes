use super::*;

const PCT_MAX: f64 = 100.0;

fn pct_of(typed: u64, total: u64) -> f64 {
    PCT_MAX * typed as f64 / total as f64
}

impl Bucket {
    /// Coverage percentage, unrounded so a near miss isn't displayed as a pass.
    pub(super) fn pct(&self) -> f64 {
        if self.total == 0 {
            PCT_MAX
        } else {
            pct_of(self.typed, self.total)
        }
    }
}

// Accessors for the checker's JSON; the checker always writes these keys.
pub(super) fn text<'a>(value: &'a Value, key: &str) -> &'a str {
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

pub(super) fn by_file(sites: &[Value]) -> BTreeMap<&str, Vec<&Value>> {
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

pub(super) fn sorted_by_position<'a>(sites: Option<&Vec<&'a Value>>) -> Vec<&'a Value> {
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
    let mut globs: HashMap<&str, Glob> = HashMap::new();
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
                        globs
                            .entry(pattern.as_str())
                            .or_insert_with(|| Glob::new(pattern))
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

// A site's line, within its notebook cell (`0` outside a notebook): lines repeat across cells.
pub(super) fn line_key(site: &Value) -> (i64, i64) {
    (int(site, "cell"), int(site, "line"))
}

// Classifies one `all_dataframe_calls` site against the typed and untyped sites, by line. By line
// and not by column: the sites record the enclosing statement's position and the call its own,
// which differ on the same line by design. Two DataFrame-shaped calls on one line are ambiguous
// under this scheme, an accepted and documented limitation.
pub(super) fn explain_status(
    site: &Value,
    typed_by_line: &HashMap<(i64, i64), &Value>,
    untyped_lines: &HashSet<(i64, i64)>,
) -> String {
    let line = line_key(site);
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
pub(super) fn usage_lines(usage: &Value) -> Vec<String> {
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

pub(super) fn files_in(sources: &Sources) -> BTreeSet<String> {
    [
        sources.all_dataframe_calls,
        sources.leg_events,
        sources.untyped_site_usage,
    ]
    .iter()
    .flat_map(|sites| sites.iter().map(|site| text(site, "file").to_string()))
    .collect()
}
