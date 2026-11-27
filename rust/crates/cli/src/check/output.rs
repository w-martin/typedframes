use super::*;

fn drop_after_chars(text: &str, n: usize) -> &str {
    text.char_indices().nth(n).map_or("", |(i, _)| &text[i..])
}

pub(super) fn print_results(
    out: &mut impl Write,
    run: &Run,
    errors: &[Value],
    format: OutputFormat,
    show_info: bool,
) -> io::Result<()> {
    let errors_only: Vec<&Value> = errors
        .iter()
        .filter(|e| !matches!(severity(e), Some("warning" | "info")))
        .collect();
    let warnings = errors
        .iter()
        .filter(|e| severity(e) == Some("warning"))
        .count();
    let color = format == OutputFormat::Text && io::stdout().is_terminal();
    let caveat = if show_info {
        low_coverage_caveat(run.total, run.typed, run.unresolved, errors.len())
    } else {
        None
    };

    if format == OutputFormat::Github {
        return print_github(out, run, errors, show_info, &caveat);
    }

    let paint = |style: &str, text: &str| {
        if color {
            format!("{style}{text}{RESET}")
        } else {
            text.to_string()
        }
    };
    if let Some(caveat) = &caveat {
        writeln!(out, "{}", paint(DIM, caveat))?;
    }
    if !errors.is_empty() {
        writeln!(out, "{}", format_text(errors, color))?;
        writeln!(out)?;
    }
    let label = if run.files == 1 { "file" } else { "files" };
    let duration = format_duration(run.elapsed);
    if !errors_only.is_empty() || warnings > 0 {
        let summary = error_summary_parts(&errors_only, warnings).join(", ");
        let message = format!(
            "\u{2717} Found {summary} in {} {label} ({duration})",
            run.files
        );
        writeln!(out, "{}", paint(BOLD_RED, &message))?;
    } else {
        let message = format!("\u{2713} Checked {} {label} in {duration}", run.files);
        writeln!(out, "{}", paint(BOLD_GREEN, &message))?;
    }
    if show_info && caveat.is_none() {
        writeln!(
            out,
            "{}",
            paint(
                DIM,
                &coverage_message(
                    run.total,
                    run.typed,
                    run.unresolved,
                    run.lost,
                    run.uncounted
                )
            )
        )?;
    }
    Ok(())
}

// GitHub Actions annotations: the coverage caveat, the errors, and the coverage notice.
fn print_github(
    out: &mut impl Write,
    run: &Run,
    errors: &[Value],
    show_info: bool,
    caveat: &Option<String>,
) -> Result<(), io::Error> {
    if let Some(caveat) = caveat {
        writeln!(
            out,
            "::notice title=typedframes DataFrame schema coverage::{}",
            drop_after_chars(caveat, 2)
        )?;
    }
    if !errors.is_empty() {
        writeln!(out, "{}", format_github(errors))?;
    }
    if show_info && caveat.is_none() {
        writeln!(
            out,
            "::notice title=typedframes DataFrame schema coverage::{}",
            drop_after_chars(
                &coverage_message(
                    run.total,
                    run.typed,
                    run.unresolved,
                    run.lost,
                    run.uncounted
                ),
                2
            )
        )?;
    }
    Ok(())
}

pub(super) fn print_json_results(
    out: &mut impl Write,
    run: &Run,
    errors: Vec<Value>,
    coverage: Option<Value>,
) -> io::Result<()> {
    let mut payload = Map::new();
    payload.insert("errors".into(), Value::Array(errors));
    payload.insert(
        "stats".into(),
        json!({"dataframes_total": run.total, "dataframes_typed": run.typed, "dataframes_unresolved": run.unresolved}),
    );
    if let Some(coverage) = coverage {
        payload.insert("coverage".into(), coverage);
    }
    writeln!(out, "{}", dumps_indent2(&Value::Object(payload)))
}

// The richer report `--coverage-detail` asked for, for text and GitHub output; under JSON the same
// detail is nested into the single document instead.
pub(super) fn print_coverage_report(
    out: &mut impl Write,
    findings: &Findings,
    root: &Path,
    detail: CoverageDetail,
) -> io::Result<()> {
    if detail == CoverageDetail::Summary {
        return Ok(());
    }
    writeln!(out)?;
    if detail == CoverageDetail::Explain {
        writeln!(out, "{}", format_explain(&findings.sources(), root))
    } else {
        writeln!(
            out,
            "{}",
            format_term_missing(&findings.per_file, &findings.untyped_sites, root)
        )
    }
}

pub(super) fn print_coverage_failures(
    out: &mut impl Write,
    failing: &[crate::coverage::Bucket],
    format: OutputFormat,
) -> io::Result<()> {
    for bucket in failing {
        let message = failure_message(bucket);
        match format {
            OutputFormat::Github => writeln!(
                out,
                "::error title=typedframes DataFrame schema coverage::{}",
                drop_after_chars(&message, 2)
            )?,
            OutputFormat::Json => eprintln!("{message}"),
            OutputFormat::Text => {
                if io::stdout().is_terminal() {
                    writeln!(out, "{BOLD_RED}{message}{RESET}")?;
                } else {
                    writeln!(out, "{message}")?;
                }
            }
        }
    }
    Ok(())
}
