//! Bug-report material for one untyped site: how the checker classified it, and a
//! redacted copy of the code around it that is classified the same way.
//!
//! The snippet is the source's own tokens with every name and string renamed, except
//! those listed in `report_names` (library and builtin names, option strings). Only the
//! code the checker looked at is kept: the enclosing function or class member, same-file
//! functions it calls, library imports, and a signature-only stub for an imported
//! function whose library the index knew. The redacted code is then run through the
//! checker; if it isn't classified exactly as the real site was, no snippet is given.

use crate::linter::{Linter, UnresolvedDataframeShapedCall};
use crate::phrase;
use crate::report_minimize::minimize;
use crate::report_names::{keeps_library_name, keeps_name, keeps_string, KEPT_IMPORT_ROOTS};
use crate::suggest::Library;
use ruff_python_ast::{self as ast, Stmt, StmtClassDef};
use ruff_python_parser::parse_module;
use ruff_source_file::LineIndex;
use ruff_text_size::Ranged;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

mod kept;
mod redact;
mod snippet;
mod verdict;

use kept::*;
use redact::*;
use snippet::*;
use verdict::*;

/// Most lines a snippet may have once reduced; a bigger scope is left for the reporter to trim.
const MAX_SNIPPET_LINES: usize = 150;

/// Most lines of redacted code worth reducing.
const MAX_UNREDUCED_LINES: usize = 1500;

/// Most extra same-file functions pulled in because the site's scope calls them.
const MAX_HELPERS: usize = 12;

/// What the checker decided about a site, with names and numbers masked so it can be
/// shown without the code.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Verdict {
    pub outcome: &'static str,
    pub kind: Option<String>,
    pub tier: Option<String>,
    pub reason: String,
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct SiteReport {
    pub name: String,
    /// Where the site is, for telling apart two files that share a name. Local only.
    pub line: usize,
    pub col: usize,
    pub var: String,
    pub verdict: Verdict,
    pub snippet: Option<String>,
    /// Why there is no snippet, when there isn't.
    pub refusal: Option<String>,
}

// Confirms the redacted snippet records an untracked frame where the original had one.
fn verify_untracked(snippet: &str, position: (usize, usize)) -> Result<(), String> {
    let linter = analyse(snippet)?;
    if linter
        .unresolved_dataframe_shaped_calls
        .iter()
        .any(|call| (call.line, call.col) == position)
    {
        Ok(())
    } else {
        Err(
            "the snippet no longer holds a frame the checker does not track where the original \
             was"
            .to_string(),
        )
    }
}

// `Name(...)` builds an object; it is listed only because a method of the object shares a
// name with a DataFrame method.
fn is_reportable_untracked(call: &UnresolvedDataframeShapedCall) -> bool {
    !call.phrase.is_empty()
        && (call.is_attribute_call || !call.name.chars().next().is_some_and(char::is_uppercase))
}

fn untracked_report(
    source: &str,
    index: &LineIndex,
    linter: &Linter,
    call: &UnresolvedDataframeShapedCall,
) -> SiteReport {
    let verdict = Verdict {
        outcome: "untracked",
        kind: None,
        tier: None,
        reason: format!(
            "it comes from {}, which the checker does not treat as a DataFrame source, but it is \
             used like a DataFrame",
            call.callee
        ),
        notes: Vec::new(),
    };
    let built = phrase::offset_of(source, index, call.line, call.col)
        .ok_or_else(|| "the frame's position could not be found".to_string())
        .and_then(|offset| {
            build_snippet(
                source,
                offset,
                linter.imported_function_libraries(),
                linter.imported_class_method_libraries(),
            )
        })
        .and_then(|(snippet, at)| {
            verify_untracked(&snippet, at)?;
            let (reduced, _) =
                minimize(&snippet, at, |text, to| verify_untracked(text, to).is_ok());
            Ok(reduced)
        });
    let (snippet, refusal) = match built {
        Ok(snippet) => (Some(snippet), None),
        Err(reason) => (None, Some(reason)),
    };
    SiteReport {
        name: call.phrase.clone(),
        line: call.line,
        col: call.col,
        var: call.var.clone(),
        verdict,
        snippet,
        refusal,
    }
}

/// A report for every frame in the file that is used like a DataFrame but comes from a call the
/// checker does not treat as a DataFrame source, so it is not tracked and has no untyped site.
/// These can include a call that merely shares a method name with pandas or polars; a plain
/// `Class(...)` construction is left out.
pub(crate) fn untracked_all(source: &str, linter: &Linter) -> Vec<SiteReport> {
    let index = LineIndex::from_source_text(source);
    linter
        .unresolved_dataframe_shaped_calls
        .iter()
        .filter(|call| is_reportable_untracked(call))
        .map(|call| untracked_report(source, &index, linter, call))
        .collect()
}

/// The report for every untyped site in the file the linter has just checked, in file order.
pub(crate) fn report_all(source: &str, linter: &Linter) -> Vec<SiteReport> {
    linter
        .untyped_sites
        .iter()
        .filter_map(|site| report_for(source, linter, &site.phrase))
        .collect()
}

/// The report for the untyped site called `name`, or `None` if this file has no such site.
pub(crate) fn report_for(source: &str, linter: &Linter, name: &str) -> Option<SiteReport> {
    let Some(site) = linter.untyped_sites.iter().find(|s| s.phrase == name) else {
        let call = linter
            .unresolved_dataframe_shaped_calls
            .iter()
            .find(|c| is_reportable_untracked(c) && c.phrase == name)?;
        return Some(untracked_report(
            source,
            &LineIndex::from_source_text(source),
            linter,
            call,
        ));
    };
    let verdict = verdict_of(linter, name)?;
    let index = LineIndex::from_source_text(source);
    let offset = phrase::offset_of(source, &index, site.line, site.col);
    let offset = offset.ok_or_else(|| "the site's position could not be found".to_string());
    let attempt = |stubs: &HashMap<String, Library>| {
        let offset = offset.clone()?;
        let (snippet, position) = build_snippet(
            source,
            offset,
            stubs,
            linter.imported_class_method_libraries(),
        )?;
        verify(&snippet, position, &verdict)?;
        Ok((snippet, position))
    };
    // A stub stands for a function that returns a frame; one the frame is passed to is
    // better kept as an import, which the stub turns into a function of this file.
    let stubs = linter.imported_function_libraries();
    let built = attempt(stubs)
        .or_else(|first: String| {
            if stubs.is_empty() {
                return Err(first);
            }
            attempt(&HashMap::new()).map_err(|_| first)
        })
        .and_then(|(snippet, position)| {
            let (reduced, _) = minimize(&snippet, position, |text, at| {
                verify(text, at, &verdict).is_ok()
            });
            if reduced.lines().count() > MAX_SNIPPET_LINES {
                return Err(format!(
                    "the code that decides this is {} lines, over the {MAX_SNIPPET_LINES}-line limit; trim it by hand",
                    reduced.lines().count()
                ));
            }
            Ok(reduced)
        });
    let (snippet, refusal) = match built {
        Ok(snippet) => (Some(snippet), None),
        Err(reason) => (None, Some(reason)),
    };
    Some(SiteReport {
        name: name.to_string(),
        line: site.line,
        col: site.col,
        var: site.var.clone(),
        verdict,
        snippet,
        refusal,
    })
}

#[cfg(test)]
mod tests;
