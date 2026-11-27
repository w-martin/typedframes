use super::*;

pub(super) fn report_nothing_to_apply(
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
pub(super) fn apply_and_report(
    out: &mut impl Write,
    requests: &[Request],
    index: Option<Arc<ProjectIndex>>,
    require_tracked: bool,
) -> io::Result<Result<usize, i32>> {
    let result: ApplyResult = apply_all(
        &crate::files(),
        &typedframes_adapters::GitWorkspace,
        requests,
        index,
        require_tracked,
    );
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
                "applied  {}  {}  {}  [{columns}]",
                item.phrase,
                place(&outcome.file, item.line, item.col, item.cell),
                item.var
            )?;
        }
        for item in &outcome.skipped {
            writeln!(
                out,
                "skipped  {}  {}",
                place(&outcome.file, item.line, item.col, item.cell),
                item.reason
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
