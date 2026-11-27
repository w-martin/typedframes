use super::*;

pub(super) fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

pub(super) fn int(value: &Value, key: &str) -> usize {
    value.get(key).and_then(Value::as_u64).unwrap_or(0) as usize
}

/// `path:line:col`, or `path cell N:line:col` for a site in a notebook, from its parts.
pub(super) fn place(file: &str, line: usize, col: usize, cell: Option<(usize, usize)>) -> String {
    match cell {
        Some((cell, cell_line)) => format!("{file} cell {cell}:{cell_line}:{col}"),
        None => format!("{file}:{line}:{col}"),
    }
}

/// `path:line:col`, or `path cell N:line:col` for a site in a notebook.
pub(super) fn location(display_path: &str, site: &Value) -> String {
    match site
        .get("cell")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
    {
        Some([cell, line]) => format!(
            "{display_path} cell {}:{}:{}",
            cell.as_u64().unwrap_or(0),
            line.as_u64().unwrap_or(0),
            int(site, "col")
        ),
        _ => format!("{display_path}:{}:{}", int(site, "line"), int(site, "col")),
    }
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
pub(super) fn diff_name(file: &Path, root: &Path) -> String {
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
        "{}  {}",
        text(suggestion, "phrase"),
        location(display_path, suggestion)
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

pub(super) fn print_suggestions(
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
                "  {}  {}  {}  {}",
                text(site, "phrase"),
                location(display_path, site),
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
