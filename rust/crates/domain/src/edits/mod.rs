//! Applying suggestions to a file's text: the edits themselves, the checks an edited file must
//! pass (it parses, keeps its imports in order, gains no errors), and notebook rewriting. Reading
//! and writing the files is the application layer's.

use crate::analysis::{linter_with_index, notebook_linter, FileContext};
use crate::errors::LintError;
use crate::suggest::{imports_in_order, without_orphaned_class_var, Edit, Suggestion};
use ruff_diagnostics::SourceMap;
use ruff_notebook::{Cell, Notebook, SourceValue};
use ruff_python_parser::{parse, parse_module, Mode, ParseOptions};
use ruff_text_size::TextSize;
use serde::Serialize;
use serde_json::Value;
use std::path::Path;

/// Which of a file's suggestions to apply.
pub enum Selection {
    /// Every suggestion whose tier is `safe`.
    SafeTier,
    /// Exactly the suggestions whose origin is at these `(line, col)` positions, whatever
    /// their tier -- what the user chose one by one.
    Origins(Vec<(usize, usize)>),
}

/// A source file's suggestions, the source they were made from, and its current errors.
pub(crate) struct Analyzed {
    pub(crate) source: String,
    /// The file as it is on disk: the same as `source`, except for a notebook, whose
    /// `source` is the concatenation of its code cells.
    pub(crate) raw: String,
    pub(crate) notebook: Option<Notebook>,
    pub(crate) suggestions: Vec<Suggestion>,
    // Line numbers removed, so inserting a line doesn't change a key.
    pub(crate) errors: Vec<String>,
}

fn error_keys(errors: &[LintError]) -> Vec<String> {
    let mut keys: Vec<String> = errors
        .iter()
        .filter(|e| e.severity == "error")
        .map(|e| format!("{}\u{0}{}", e.code, strip_line_numbers(&e.message)))
        .collect();
    keys.sort();
    keys
}

fn ends_with_word(text: &str, word: &str) -> bool {
    text.strip_suffix(word)
        .is_some_and(|before| !before.ends_with(|c: char| c.is_alphanumeric() || c == '_'))
}

// Drops the digits of `line 12` / `file.py:12` references, which shift on insertion.
fn strip_line_numbers(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut rest = message;
    while let Some(start) = rest.find(|c: char| c.is_ascii_digit()) {
        let (before, digits_on) = rest.split_at(start);
        out.push_str(before);
        let length = digits_on
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(digits_on.len());
        let is_line_number =
            ends_with_word(&out, "line ") || out.ends_with(".py:") || out.ends_with(".ipynb:");
        if !is_line_number {
            out.push_str(&digits_on[..length]);
        }
        rest = &digits_on[length..];
    }
    out.push_str(rest);
    out
}

/// Errors present in `after` beyond those in `before`, counting duplicates.
fn new_errors(before: &[String], after: &[String]) -> usize {
    let mut remaining: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for key in before {
        *remaining.entry(key).or_default() += 1;
    }
    after
        .iter()
        .filter(|key| match remaining.get_mut(key.as_str()) {
            Some(count) if *count > 0 => {
                *count -= 1;
                false
            }
            _ => true,
        })
        .count()
}

fn check_source(
    linter: &mut crate::linter::Linter,
    source: &str,
    path: &Path,
    notebook: bool,
) -> Result<Vec<LintError>, String> {
    let checked = if notebook {
        linter.check_notebook_internal(source, path)
    } else {
        linter.check_file_internal(source, path)
    };
    checked.map_err(|e| e.to_string())
}

/// Where each edit moves the notebook's cell boundaries, in the form `Notebook::update` reads.
fn source_map_for(edits: &[Edit]) -> SourceMap {
    let mut ordered: Vec<&Edit> = edits.iter().collect();
    ordered.sort_by_key(|e| (e.start, e.end));
    ordered.dedup_by(|a, b| a.start == b.start && a.end == b.end && a.replacement == b.replacement);
    let mut map = SourceMap::default();
    let mut shift = 0isize;
    let at = |offset: usize, shift: isize| TextSize::try_from((offset as isize + shift) as usize);
    for edit in ordered {
        let (Ok(start), Ok(end)) = (TextSize::try_from(edit.start), TextSize::try_from(edit.end))
        else {
            continue;
        };
        if let Ok(dest) = at(edit.start, shift) {
            map.push_marker(start, dest);
        }
        shift += edit.replacement.len() as isize - (edit.end - edit.start) as isize;
        if let Ok(dest) = at(edit.end, shift) {
            map.push_marker(end, dest);
        }
    }
    map
}

fn source_text(source: &SourceValue) -> String {
    match source {
        SourceValue::String(text) => text.clone(),
        SourceValue::StringArray(lines) => lines.concat(),
    }
}

/// `raw`, a notebook file, with only the `source` of the code cells `edits` changed replaced, so
/// everything else (key order, ids, outputs, indentation) stays as it was.
fn rewritten_notebook(
    mut notebook: Notebook,
    edits: &[Edit],
    edited_source: String,
    raw: &str,
) -> Result<String, String> {
    notebook.update(&source_map_for(edits), edited_source);
    let mut document: Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    let cells = document
        .get_mut("cells")
        .and_then(Value::as_array_mut)
        .filter(|cells| cells.len() == notebook.cells().len())
        .ok_or("its cells do not match the parsed notebook")?;
    for (cell, raw_cell) in notebook.cells().iter().zip(cells) {
        let Cell::Code(code) = cell else { continue };
        let before = match raw_cell.get("source") {
            Some(Value::String(text)) => text.clone(),
            Some(Value::Array(lines)) => lines.iter().filter_map(Value::as_str).collect(),
            _ => String::new(),
        };
        if before != source_text(&code.source) {
            raw_cell["source"] = serde_json::to_value(&code.source).map_err(|e| e.to_string())?;
        }
    }
    let indent = raw
        .lines()
        .nth(1)
        .map(|line| line.len() - line.trim_start().len())
        .filter(|width| *width > 0)
        .unwrap_or(1);
    let indent = vec![b' '; indent];
    let mut out = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(
        &mut out,
        serde_json::ser::PrettyFormatter::with_indent(&indent),
    );
    document
        .serialize(&mut serializer)
        .map_err(|e| e.to_string())?;
    if raw.ends_with('\n') {
        out.push(b'\n');
    }
    String::from_utf8(out).map_err(|e| e.to_string())
}

pub(crate) fn analyze(
    context: &FileContext,
    display_path: &str,
    raw: String,
    notebook: Option<Notebook>,
) -> Result<Analyzed, String> {
    let (mut linter, source) = match &notebook {
        Some(notebook) => {
            let source = notebook.source_code().to_string();
            (notebook_linter(context, &source), source)
        }
        None => (linter_with_index(context, &raw), raw.clone()),
    };
    linter.collect_suggestions = true;
    linter.suggest_display_path = display_path.to_string();
    let errors = check_source(&mut linter, &source, context.path, notebook.is_some())?;
    Ok(Analyzed {
        source,
        raw,
        notebook,
        suggestions: std::mem::take(&mut linter.suggestions),
        errors: error_keys(&errors),
    })
}

#[derive(Serialize, Clone, Debug)]
pub struct AppliedSuggestion {
    pub phrase: String,
    pub line: usize,
    pub col: usize,
    pub var: String,
    pub columns: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cell: Option<(usize, usize)>,
}

#[derive(Serialize, Clone, Debug)]
pub struct SkippedSuggestion {
    pub line: usize,
    pub col: usize,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cell: Option<(usize, usize)>,
}

#[derive(Serialize, Debug, Default)]
pub struct FileOutcome {
    pub file: String,
    pub applied: Vec<AppliedSuggestion>,
    pub skipped: Vec<SkippedSuggestion>,
    pub errors_before: usize,
    pub errors_after: usize,
    pub written: bool,
    pub error: Option<String>,
}

/// `edits` applied to `source`. Edits are insertions or replacements at distinct places;
/// later offsets go first so earlier ones stay valid.
pub fn apply_edits(source: &str, edits: &[Edit]) -> Option<String> {
    let mut ordered: Vec<&Edit> = edits.iter().collect();
    ordered.sort_by(|a, b| b.start.cmp(&a.start).then(b.end.cmp(&a.end)));
    // Suggestions that each carry the same shared edit (an import) apply it once.
    ordered.dedup_by(|a, b| a.start == b.start && a.end == b.end && a.replacement == b.replacement);
    let mut out = source.to_string();
    let mut previous_start = usize::MAX;
    for edit in ordered {
        if edit.end > previous_start
            || edit.start > edit.end
            || !out.is_char_boundary(edit.start)
            || !out.is_char_boundary(edit.end)
            || edit.end > out.len()
        {
            return None;
        }
        out.replace_range(edit.start..edit.end, &edit.replacement);
        previous_start = edit.start;
    }
    Some(out)
}

/// Works out what to do for one file. Nothing is written here.
pub fn plan(
    context: &FileContext,
    display_path: &str,
    selection: &Selection,
    raw: String,
    notebook: Option<Notebook>,
) -> (FileOutcome, Option<String>) {
    let mut outcome = FileOutcome {
        file: display_path.to_string(),
        ..FileOutcome::default()
    };
    let analyzed = match analyze(context, display_path, raw, notebook) {
        Ok(analyzed) => analyzed,
        Err(error) => {
            outcome.error = Some(error);
            return (outcome, None);
        }
    };
    outcome.errors_before = analyzed.errors.len();
    outcome.errors_after = analyzed.errors.len();
    apply_chosen(context, selection, outcome, analyzed)
}

// Applies the chosen suggestions to the analysed file once the edited source passes every check.
fn apply_chosen(
    context: &FileContext<'_>,
    selection: &Selection,
    mut outcome: FileOutcome,
    analyzed: Analyzed,
) -> (FileOutcome, Option<String>) {
    let chosen = chosen_suggestions(selection, &analyzed.suggestions, &mut outcome);
    if chosen.is_empty() {
        return (outcome, None);
    }
    let edits: Vec<Edit> = chosen
        .iter()
        .flat_map(|s| s.edits.iter().cloned())
        .collect();
    let skip_all = |outcome: &mut FileOutcome, reason: String| {
        for s in &chosen {
            outcome.skipped.push(SkippedSuggestion {
                line: s.line,
                col: s.col,
                reason: reason.clone(),
                cell: s.cell,
            });
        }
    };
    let edited = match checked_edit(context, &analyzed, &edits, &mut outcome) {
        Ok(edited) => edited,
        Err(reason) => {
            skip_all(&mut outcome, reason);
            return (outcome, None);
        }
    };
    outcome.applied = chosen
        .iter()
        .map(|s| AppliedSuggestion {
            phrase: s.phrase.clone(),
            line: s.line,
            col: s.col,
            var: s.var.clone(),
            columns: s.columns.clone(),
            cell: s.cell,
        })
        .collect();
    let text = match analyzed.notebook {
        Some(notebook) => match rewritten_notebook(notebook, &edits, edited, &analyzed.raw) {
            Ok(text) => text,
            Err(error) => {
                outcome.applied.clear();
                skip_all(
                    &mut outcome,
                    format!("the notebook could not be rewritten: {error}"),
                );
                return (outcome, None);
            }
        },
        None => edited,
    };
    (outcome, Some(text))
}

// The suggestions `selection` picks; a requested one that no longer exists is skipped on
// `outcome`.
fn chosen_suggestions<'a>(
    selection: &Selection,
    suggestions: &'a [Suggestion],
    outcome: &mut FileOutcome,
) -> Vec<&'a Suggestion> {
    match selection {
        Selection::SafeTier => suggestions.iter().filter(|s| s.tier == "safe").collect(),
        Selection::Origins(wanted) => {
            let mut chosen = Vec::new();
            for (line, col) in wanted {
                match suggestions
                    .iter()
                    .find(|s| s.line == *line && s.col == *col)
                {
                    Some(suggestion) => chosen.push(suggestion),
                    None => outcome.skipped.push(SkippedSuggestion {
                        line: *line,
                        col: *col,
                        reason: "no longer suggested: the file changed since it was analysed"
                            .to_string(),
                        cell: None,
                    }),
                }
            }
            chosen
        }
    }
}

// The edited source, once it parses, keeps its imports in order and gains no errors; otherwise
// why the edits can't be made. Records the edited file's error count on `outcome`.
fn checked_edit(
    context: &FileContext<'_>,
    analyzed: &Analyzed,
    edits: &[Edit],
    outcome: &mut FileOutcome,
) -> Result<String, String> {
    let Some(edited) = apply_edits(&analyzed.source, edits) else {
        return Err("its edits overlap or don't fit the file".to_string());
    };
    let (edited, is_notebook, parses) = plan_edited_analyzed_notebook(analyzed, edited);
    if !parses {
        return Err("the edited file would not parse".to_string());
    }
    // An added import must not turn an ordered import block into one `ruff` rejects.
    if !is_notebook && imports_in_order(&analyzed.source) && !imports_in_order(&edited) {
        return Err("the edited file's imports would no longer be in order".to_string());
    }
    // Check the edited source exactly as the original was checked.
    let mut linter = if is_notebook {
        notebook_linter(context, &edited)
    } else {
        linter_with_index(context, &edited)
    };
    let after = check_source(&mut linter, &edited, context.path, is_notebook)
        .map(|errors| error_keys(&errors))
        .map_err(|e| format!("the edited file failed to check: {e}"))?;
    outcome.errors_after = after.len();
    let introduced = new_errors(&analyzed.errors, &after);
    if introduced > 0 {
        return Err(format!(
            "re-checking the edited file found {introduced} new error{}",
            if introduced == 1 { "" } else { "s" }
        ));
    }
    Ok(edited)
}

fn plan_edited_analyzed_notebook(analyzed: &Analyzed, edited: String) -> (String, bool, bool) {
    let edited = if analyzed.notebook.is_some() {
        edited
    } else {
        without_orphaned_class_var(&analyzed.source, &edited)
    };
    let is_notebook = analyzed.notebook.is_some();
    let parses = if is_notebook {
        parse(&edited, ParseOptions::from(Mode::Ipython)).is_ok()
    } else {
        parse_module(&edited).is_ok()
    };
    (edited, is_notebook, parses)
}

#[cfg(test)]
mod tests;
