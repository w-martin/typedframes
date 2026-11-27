use super::*;

// Applies `--lenient-ingest` (untracked-dataframe downgraded to info) and `--no-warnings` /
// `--no-info` filtering. The checker already reports untracked-dataframe as a warning.
pub(super) fn apply_diagnostic_policy(mut errors: Vec<Value>, args: &CheckArgs) -> Vec<Value> {
    if args.lenient_ingest {
        for error in &mut errors {
            if str_of(error, "code") == "untracked-dataframe" {
                if let Some(map) = error.as_object_mut() {
                    map.insert("severity".into(), Value::String("info".into()));
                }
            }
        }
    }
    if args.no_warnings {
        errors.retain(|e| severity(e) != Some("warning"));
    }
    if args.no_info {
        errors.retain(|e| severity(e) != Some("info"));
    } else if !args.verbose {
        collapse_assumed_constants(&mut errors);
    }
    errors
}

// One note per run rather than one per `self.X` constant: a codebase that names its columns through
// class attributes would otherwise drown the real findings. `--verbose` keeps them all.
fn collapse_assumed_constants(errors: &mut Vec<Value>) {
    let is_note = |e: &Value| str_of(e, "code") == "assumed-constant";
    let count = errors.iter().filter(|e| is_note(e)).count();
    if count < 2 {
        return;
    }
    let first = errors.iter().position(is_note).unwrap_or(0);
    // Anchored at the first note in path order, whatever order the files were checked in.
    let key = |e: &Value| {
        (
            str_of(e, "file").to_string(),
            int_of(e, "line"),
            int_of(e, "col"),
        )
    };
    let mut summary = errors
        .iter()
        .filter(|e| is_note(e))
        .min_by_key(|e| key(e))
        .cloned()
        .unwrap_or_else(|| errors[first].clone());
    let files: std::collections::HashSet<&str> = errors
        .iter()
        .filter(|e| is_note(e))
        .map(|e| str_of(e, "file"))
        .collect();
    let across = if files.len() > 1 {
        format!(" across {} files", files.len())
    } else {
        String::new()
    };
    summary["message"] = Value::String(format!(
        "{count} `self.<NAME>` constants{across} are read assuming no subclass in another file \
         overrides them; declare each `<NAME>: Final = ...` to make that certain (--verbose lists them)"
    ));
    errors.retain(|e| !is_note(e));
    errors.insert(first.min(errors.len()), summary);
}
