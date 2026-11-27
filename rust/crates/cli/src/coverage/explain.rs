use super::*;

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
        let typed_by_line: HashMap<(i64, i64), &Value> = typed
            .get(name)
            .into_iter()
            .flatten()
            .map(|site| (line_key(site), *site))
            .collect();
        let untyped_here: &[&Value] = untyped.get(name).map_or(&[], Vec::as_slice);
        let untyped_lines: HashSet<(i64, i64)> = untyped_here.iter().map(|s| line_key(s)).collect();
        let mut sites_per_line: HashMap<(i64, i64), usize> = HashMap::new();
        for site in untyped_here {
            *sites_per_line.entry(line_key(site)).or_default() += 1;
        }
        let phrase_by_line: HashMap<(i64, i64), &str> = untyped_here
            .iter()
            .filter(|s| !text(s, "phrase").is_empty() && sites_per_line[&line_key(s)] == 1)
            .map(|s| (line_key(s), text(s, "phrase")))
            .collect();

        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(relative_posix(name, root));
        for site in sorted_by_position(calls.get(name)) {
            let mut status = explain_status(site, &typed_by_line, &untyped_lines);
            if let Some(phrase) = phrase_by_line.get(&line_key(site)) {
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
        let typed_by_line: HashMap<(i64, i64), &Value> = typed
            .get(name)
            .into_iter()
            .flatten()
            .map(|site| (line_key(site), *site))
            .collect();
        let untyped_lines: HashSet<(i64, i64)> = untyped
            .get(name)
            .into_iter()
            .flatten()
            .map(|s| line_key(s))
            .collect();
        let call_entries: Vec<Value> = sorted_by_position(calls.get(name))
            .into_iter()
            .map(|site| {
                let line = line_key(site);
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
