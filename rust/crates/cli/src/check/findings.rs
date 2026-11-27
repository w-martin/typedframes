use super::*;

// `{**item, "file": file}`: the file goes last unless the item already had one.
pub(super) fn tagged(item: &Value, file: &str) -> Value {
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
        findings.unresolved += stats["dataframes_unresolved"].as_u64().unwrap_or(0);
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
    // Distinct frames, per file, that were tracked and then fell out of tracking. Not part of the
    // coverage ratio, whose denominator is the frames the checker found at their creation.
    pub(super) fn lost_frames(&self) -> u64 {
        self.leg_events
            .iter()
            .filter(|e| matches!(str_of(e, "outcome"), "untracked" | "unresolved"))
            .map(|e| (str_of(e, "file"), str_of(e, "var")))
            .collect::<std::collections::BTreeSet<_>>()
            .len() as u64
    }

    // Load calls the scan found that no tracked or untracked site accounts for (`pd.read_csv(..)`
    // as an argument or inside an expression): frames the percentage knows nothing about. Matched
    // by line, as `--coverage-detail=explain` does.
    pub(super) fn uncounted_loads(&self) -> u64 {
        let line = |site: &Value| {
            (
                str_of(site, "file").to_string(),
                int_of(site, "cell"),
                int_of(site, "line"),
            )
        };
        let counted: std::collections::HashSet<_> = self
            .typed_sites
            .iter()
            .chain(self.untyped_sites.iter())
            .map(line)
            .collect();
        self.all_dataframe_calls
            .iter()
            .filter(|site| !counted.contains(&line(site)))
            .count() as u64
    }

    pub(super) fn sources(&self) -> Sources<'_> {
        Sources {
            all_dataframe_calls: &self.all_dataframe_calls,
            typed_sites: &self.typed_sites,
            untyped_sites: &self.untyped_sites,
            leg_events: &self.leg_events,
            untyped_site_usage: &self.untyped_site_usage,
        }
    }
}

pub(super) fn severity(error: &Value) -> Option<&str> {
    error.get("severity").and_then(Value::as_str)
}

pub(super) fn str_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

pub(super) fn int_of(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(0)
}

// `line:col`, or `cell N:line:col` for a notebook error.
pub(super) fn error_location(error: &Value) -> String {
    let prefix = error
        .get("cell")
        .and_then(Value::as_i64)
        .map(|cell| format!("cell {cell}:"))
        .unwrap_or_default();
    format!("{prefix}{}:{}", int_of(error, "line"), int_of(error, "col"))
}
