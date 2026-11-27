use super::*;

const MAX_NAMED_BLOCKERS: usize = 4;

// The edit that adds `keyword=[..]` to the load call, the source it makes, and the keyword.
fn in_call_edit(source: &str, site: &InCall, quoted: &[String]) -> (Edit, String, &'static str) {
    let text = format!(", {}=[{}]", site.keyword, quoted.join(", "));
    let mut new_source = String::with_capacity(source.len() + text.len());
    new_source.push_str(&source[..site.offset]);
    new_source.push_str(&text);
    new_source.push_str(&source[site.offset..]);
    let edit = Edit {
        start: site.offset,
        end: site.offset,
        replacement: text,
    };
    (edit, new_source, site.keyword)
}

// The edit that selects the columns on the line after the load (`var = var[[..]]`, or
// `var.select(..)` for polars), and the source it makes.
fn selection_line_edit(
    origin: &Origin,
    library: Library,
    insertion: &InsertionPoint,
    quoted: &[String],
) -> (Edit, String, &'static str) {
    let expression = match library {
        Library::Pandas => format!("{}[[{}]]", origin.var, quoted.join(", ")),
        Library::Polars => format!("{}.select({})", origin.var, quoted.join(", ")),
    };
    let replacement = format!(
        "{}{} = {expression}{}",
        insertion.indent, origin.var, insertion.newline
    );
    let mut new_source = String::with_capacity(origin.source.len() + replacement.len());
    new_source.push_str(&origin.source[..insertion.offset]);
    if insertion.leading_newline {
        new_source.push('\n');
    }
    new_source.push_str(&replacement);
    new_source.push_str(&origin.source[insertion.offset..]);
    let edit_text = if insertion.leading_newline {
        format!("\n{replacement}")
    } else {
        replacement
    };
    let edit = Edit {
        start: insertion.offset,
        end: insertion.offset,
        replacement: edit_text,
    };
    (edit, new_source, "select")
}

/// `Ok` with a suggestion, or `Err` with the reason none could be made.
pub(crate) fn suggest_selection(
    origin: &Origin,
    usage: &ColumnUsage,
) -> Result<Suggestion, String> {
    let library = library_of(origin)?;
    let in_call = in_call_selection(origin, library);
    let (insertion, columns, notes) = selection_insertion(origin, usage, library, &in_call)?;

    let quoted: Vec<String> = columns
        .iter()
        .map(|c| serde_json::to_string(c).unwrap_or_else(|_| format!("\"{c}\"")))
        .collect();
    let (edit, new_source, kind) = match &in_call {
        Some(site) => in_call_edit(origin.source, site, &quoted),
        None => selection_line_edit(origin, library, &insertion, &quoted),
    };

    let (lines, through) = access_lines_and_helpers(usage);
    let reason = format!(
        "only {} {} read later (line{} {}{through})",
        columns
            .iter()
            .map(|c| format!("'{c}'"))
            .collect::<Vec<_>>()
            .join(", "),
        if columns.len() == 1 { "is" } else { "are" },
        if lines.len() == 1 { "" } else { "s" },
        lines
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(", "),
    );

    Ok(Suggestion {
        phrase: String::new(),
        kind,
        line: origin.line,
        col: origin.col,
        var: origin.var.to_string(),
        tier: if notes.is_empty() { "safe" } else { "review" },
        columns,
        reason,
        notes,
        edits: vec![edit],
        diff: render_unified_diff(origin.display_path, origin.source, &new_source),
        cell: None,
    })
}

fn access_lines_and_helpers(usage: &ColumnUsage) -> (Vec<usize>, String) {
    let mut lines: Vec<usize> = usage.accesses.iter().map(|a| a.line).collect();
    lines.sort_unstable();
    lines.dedup();
    let mut helpers: Vec<&str> = usage
        .accesses
        .iter()
        .filter_map(|a| a.via.as_deref())
        .collect();
    helpers.sort_unstable();
    helpers.dedup();
    let (callers, helpers): (Vec<&str>, Vec<&str>) = helpers
        .into_iter()
        .partition(|v| v.starts_with("callers of "));
    let through = if helpers.is_empty() {
        String::new()
    } else {
        format!(
            "; some are read inside {}",
            helpers
                .iter()
                .map(|h| format!("`{h}()`"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let through = if callers.is_empty() {
        through
    } else {
        format!("{through}; read by the {}", callers.join(", "))
    };
    (lines, through)
}

fn selection_insertion(
    origin: &Origin<'_>,
    usage: &ColumnUsage,
    library: Library,
    in_call: &Option<InCall>,
) -> Result<(InsertionPoint, Vec<String>, Vec<String>), String> {
    let insertion = insertion_point(origin)?;
    let mut blockers: Vec<&OtherUse> = usage
        .other_uses
        .iter()
        .filter(|u| crate::column_usage::blocks_narrowing(u))
        .collect();
    blockers.sort_by_key(|u| u.consumes == crate::column_usage::Consumption::None);
    if !blockers.is_empty() {
        // Name each use up to a handful, so the reason can be acted on without a second run.
        let named = blockers
            .iter()
            .take(MAX_NAMED_BLOCKERS)
            .map(|u| {
                let callee = u
                    .callee
                    .map_or_else(String::new, |class| format!(", {class}"));
                format!("{}{callee} (line {})", u.description, u.line)
            })
            .collect::<Vec<_>>()
            .join("; ");
        let more = match blockers.len().saturating_sub(MAX_NAMED_BLOCKERS) {
            0 => String::new(),
            n => format!(", and {n} more"),
        };
        return Err(format!("can't see everything done with it: {named}{more}"));
    }
    if usage.accesses.is_empty() {
        return Err("no column is read from it later in this scope".to_string());
    }
    let mut columns: Vec<String> = Vec::new();
    let mut notes = selection_columns(origin, usage, library, in_call, &mut columns);
    let mut methods: Vec<&str> = Vec::new();
    for via in usage.accesses.iter().filter_map(|a| a.via.as_deref()) {
        if via.starts_with("self.") && !methods.contains(&via) {
            methods.push(via);
        }
    }
    if !methods.is_empty() {
        let names: Vec<String> = methods.iter().map(|m| format!("`{m}()`")).collect();
        notes.push(format!(
            "assumes {} is not overridden in a subclass in another file, which this scan \
             can't see",
            names.join(", ")
        ));
    }
    if let Some(via) = usage
        .accesses
        .iter()
        .filter_map(|a| a.via.as_deref())
        .find(|v| v.starts_with("callers of "))
    {
        notes.push(format!(
            "the frame is returned, so these columns are the ones read by the {via} seen \
             under the scanned path; callers outside it, or reaching the function dynamically, \
             can't be seen"
        ));
    }
    if !origin.function_local {
        notes.push(
            "module-level variable: another module may import it and read columns this scan \
             can't see"
                .to_string(),
        );
    }
    Ok((insertion, columns, notes))
}

fn selection_columns(
    origin: &Origin<'_>,
    usage: &ColumnUsage,
    library: Library,
    in_call: &Option<InCall>,
    columns: &mut Vec<String>,
) -> Vec<String> {
    for access in &usage.accesses {
        if !columns.contains(&access.column) {
            columns.push(access.column.clone());
        }
    }

    let mut notes: Vec<String> = Vec::new();
    // A name read once, nowhere else, a couple of edits from another column the project knows,
    // is as likely a typo as a column: the selection would write it down as if it were real.
    for column in &*columns {
        let reads = usage
            .accesses
            .iter()
            .filter(|a| &a.column == column)
            .count();
        let quoted_in_file = origin.source.matches(&format!("\"{column}\"")).count()
            + origin.source.matches(&format!("'{column}'")).count();
        if reads != 1 || quoted_in_file > 1 {
            continue;
        }
        let near = columns
            .iter()
            .map(String::as_str)
            .chain(origin.known_columns.iter().map(String::as_str))
            .find(|other| {
                other != column
                    && column.chars().count() >= 5
                    && other.chars().count() >= 5
                    && crate::typo::levenshtein(column, other) <= 2
                    && crate::typo::levenshtein(column, other) * 2
                        <= column.chars().count().max(other.chars().count())
            });
        if let Some(other) = near {
            notes.push(format!(
                "'{column}' is read once and nowhere else in this file, and is a couple of edits \
                 from the known column '{other}'; if it is a misspelling the selection would keep it"
            ));
        }
    }
    for column in &*columns {
        let mut reads = usage.accesses.iter().filter(|a| &a.column == column);
        if reads.clone().all(|a| a.conditional) {
            let line = reads.next().map_or(0, |a| a.line);
            notes.push(format!(
                "'{column}' is only read conditionally (line {line}); if the data lacks it, \
                 the selection would now fail"
            ));
        }
    }
    let outputs: Vec<&OtherUse> = usage
        .other_uses
        .iter()
        .filter(|u| {
            u.description
                .starts_with(crate::column_usage::OUTPUT_USE_PREFIX)
        })
        .collect();
    if let Some(first) = outputs.first() {
        notes.push(format!(
            "the frame is displayed (line {}); the output will show only the selected columns",
            first.line
        ));
    }
    if library == Library::Pandas && usage.wrote_columns && in_call.is_none() {
        notes.push(
            "columns are assigned into the frame later; on older pandas, assigning into a \
             column selection can raise SettingWithCopyWarning"
                .to_string(),
        );
    }
    notes
}

/// Where to add a column list to the load call itself.
struct InCall {
    /// The keyword to add: `usecols` or `columns`.
    pub(super) keyword: &'static str,
    /// Just after the call's last argument.
    pub(super) offset: usize,
}

// `pd.read_csv(path)` and the like can take the column list directly, which avoids
// reading the other columns at all. Only for a bare `read_csv`/`read_parquet` call whose
// options can't refer to a column outside the list: one positional path plus keywords
// from a short list of harmless ones.
fn in_call_selection(origin: &Origin, library: Library) -> Option<InCall> {
    let Some(Expr::Call(call)) = origin.value else {
        return None;
    };
    let name = match &*call.func {
        Expr::Attribute(a) => match &*a.value {
            Expr::Name(base)
                if matches!(
                    origin.libraries.modules.get(base.id.as_str()),
                    Some(ModuleKind::Library(l)) if *l == library
                ) =>
            {
                a.attr.as_str()
            }
            _ => return None,
        },
        Expr::Name(n) if origin.libraries.names.get(n.id.as_str()) == Some(&library) => {
            n.id.as_str()
        }
        _ => return None,
    };
    let (keyword, harmless): (&'static str, &[&str]) = match (library, name) {
        (Library::Pandas, "read_csv") => (
            "usecols",
            &[
                "sep",
                "delimiter",
                "encoding",
                "encoding_errors",
                "nrows",
                "low_memory",
                "engine",
                "compression",
                "na_values",
                "keep_default_na",
                "decimal",
                "thousands",
            ],
        ),
        (Library::Pandas, "read_parquet") => ("columns", &["engine", "dtype_backend"]),
        (Library::Polars, "read_csv") => (
            "columns",
            &[
                "separator",
                "encoding",
                "n_rows",
                "infer_schema_length",
                "null_values",
                "ignore_errors",
            ],
        ),
        (Library::Polars, "read_parquet") => ("columns", &["n_rows", "parallel", "use_pyarrow"]),
        _ => return None,
    };
    let arguments = &call.arguments;
    if arguments.args.len() != 1 || matches!(arguments.args[0], Expr::Starred(_)) {
        return None;
    }
    let all_harmless = arguments.keywords.iter().all(|k| {
        k.arg
            .as_ref()
            .is_some_and(|a| harmless.contains(&a.as_str()))
    });
    if !all_harmless {
        return None;
    }
    let last_end = arguments
        .keywords
        .iter()
        .map(|k| k.range().end())
        .chain(arguments.args.iter().map(|a| a.range().end()))
        .max()?
        .to_usize();
    Some(InCall {
        keyword,
        offset: after_closing_parens(origin.source, last_end, call.range().end().to_usize()),
    })
}

// An argument written as `(path)` ends before its own closing parenthesis, so step over
// those; the call's own closing parenthesis is the last character before `call_end`.
fn after_closing_parens(source: &str, from: usize, call_end: usize) -> usize {
    let mut position = from;
    loop {
        let rest = &source[position..call_end.saturating_sub(1).max(position)];
        let gap = rest.len() - rest.trim_start().len();
        if rest.trim_start().starts_with(')') {
            position += gap + 1;
        } else {
            return position;
        }
    }
}

/// Whether `stmt` already narrows `var` to named columns: `var = var[["a", "b"]]` or
/// `var = var.select("a", "b")`. Running the suggestion again on code it has already
/// changed must find nothing to do.
pub(crate) fn is_column_selection_of(stmt: &Stmt, var: &str) -> bool {
    let Stmt::Assign(assign) = stmt else {
        return false;
    };
    let [Expr::Name(target)] = assign.targets.as_slice() else {
        return false;
    };
    if target.id.as_str() != var {
        return false;
    }
    let is_var = |expr: &Expr| matches!(expr, Expr::Name(n) if n.id.as_str() == var);
    match &*assign.value {
        Expr::Subscript(subscript) => {
            is_var(&subscript.value)
                && matches!(&*subscript.slice, Expr::List(_))
                && crate::ast_extract::extract_string_list(&subscript.slice).is_some()
        }
        Expr::Call(call) => {
            matches!(&*call.func, Expr::Attribute(a) if a.attr.as_str() == "select" && is_var(&a.value))
                && !call.arguments.args.is_empty()
                && call.arguments.keywords.is_empty()
                && call
                    .arguments
                    .args
                    .iter()
                    .all(|a| matches!(a, Expr::StringLiteral(_)))
        }
        _ => false,
    }
}
