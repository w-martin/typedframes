use super::*;

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

pub(super) fn error_summary_parts(errors_only: &[&Value], warnings: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let errors = errors_only.len();
    let label = if errors == 1 { "error" } else { "errors" };
    parts.push(format!("{errors} {label}"));
    if warnings > 0 {
        let label = if warnings == 1 { "warning" } else { "warnings" };
        parts.push(format!("{warnings} {label}"));
    }
    parts
}

/// A line noting the diagnostics below only reflect what little could be tracked: printed first
/// when under half the DataFrames the checker saw had a concrete column list. `None` when there is
/// nothing below it to caveat.
pub(crate) fn low_coverage_caveat(
    total: u64,
    typed: u64,
    unresolved: u64,
    printed: usize,
) -> Option<String> {
    if printed == 0 || total == 0 || typed as f64 / total as f64 >= LOW_COVERAGE_THRESHOLD {
        return None;
    }
    let pct = round_half_even(100.0 * typed as f64 / total as f64);
    let (noun, verb) = if printed == 1 {
        ("finding", "covers")
    } else {
        ("findings", "cover")
    };
    let unresolved_note = if unresolved > 0 {
        format!(", {unresolved} annotated with an unresolved schema")
    } else {
        String::new()
    };
    Some(format!(
        "\u{2139} {typed}/{total} DataFrames had column info ({pct}%){unresolved_note} \u{2014} the {printed} {noun} below only {verb} what little could be tracked"
    ))
}

/// The low-key coverage summary line: how much information the checker had, not a validation
/// result.
pub(crate) fn coverage_message(
    total: u64,
    typed: u64,
    unresolved: u64,
    lost: u64,
    uncounted: u64,
) -> String {
    if total == 0 {
        return "\u{2139} No DataFrames with recognized loads/schemas found to check".to_string();
    }
    let pct = round_half_even(100.0 * typed as f64 / total as f64);
    let unresolved_note = if unresolved > 0 {
        format!(", {unresolved} annotated with an unresolved schema")
    } else {
        String::new()
    };
    let lost_note = if lost > 0 {
        let (noun, verb) = if lost == 1 {
            ("frame", "lost its")
        } else {
            ("frames", "lost their")
        };
        format!(
            ", {lost} derived {noun} {verb} columns later (`--coverage-detail=explain` lists them)"
        )
    } else {
        String::new()
    };
    let uncounted_note = if uncounted > 0 {
        let noun = if uncounted == 1 { "load" } else { "loads" };
        format!(", not counting {uncounted} {noun} inside other expressions")
    } else {
        String::new()
    };
    format!(
        "\u{2139} {typed}/{total} DataFrames had column info ({pct}%){unresolved_note}{lost_note}{uncounted_note} \u{2014} DataFrame schema coverage, not a pass/fail result"
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
