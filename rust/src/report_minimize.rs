//! Shrinks a redacted snippet to the code that decides how the checker classified the site.
//!
//! Statements and parameters are deleted one at a time, last to first, and a deletion is
//! kept only if `keeps` still accepts the result, so what remains is what the verdict
//! depends on. A statement that holds the site is never deleted, only searched inside.

use crate::phrase;
use ruff_python_ast::{ExceptHandler, Stmt};
use ruff_python_parser::parse_module;
use ruff_source_file::LineIndex;
use ruff_text_size::Ranged;

/// Rounds of deleting; a deletion can make an earlier statement deletable.
const MAX_PASSES: usize = 3;

struct Cut {
    start: usize,
    end: usize,
    replacement: String,
    // A whole statement, as opposed to a parameter.
    statement: bool,
}

fn child_suites(stmt: &Stmt) -> Vec<&[Stmt]> {
    match stmt {
        Stmt::FunctionDef(f) => vec![&f.body],
        Stmt::ClassDef(c) => vec![&c.body],
        Stmt::If(i) => std::iter::once(i.body.as_slice())
            .chain(i.elif_else_clauses.iter().map(|c| c.body.as_slice()))
            .collect(),
        Stmt::For(f) => vec![&f.body, &f.orelse],
        Stmt::While(w) => vec![&w.body, &w.orelse],
        Stmt::With(w) => vec![&w.body],
        Stmt::Try(t) => {
            let mut suites: Vec<&[Stmt]> = vec![&t.body];
            for handler in &t.handlers {
                let ExceptHandler::ExceptHandler(h) = handler;
                suites.push(&h.body);
            }
            suites.push(&t.orelse);
            suites.push(&t.finalbody);
            suites
        }
        Stmt::Match(m) => m.cases.iter().map(|c| c.body.as_slice()).collect(),
        _ => Vec::new(),
    }
}

// The whole lines of `start..end` when nothing else shares them, so a deletion leaves no
// blank line behind; otherwise just the range.
fn line_bounds(text: &str, start: usize, end: usize) -> (usize, usize) {
    let line_start = text[..start].rfind('\n').map_or(0, |p| p + 1);
    let line_end = text[end..].find('\n').map_or(text.len(), |p| end + p + 1);
    let alone = text[line_start..start].trim().is_empty() && text[end..line_end].trim().is_empty();
    if alone {
        (line_start, line_end)
    } else {
        (start, end)
    }
}

fn statement_cut(text: &str, stmt: &Stmt, sole: bool) -> Option<Cut> {
    if sole && matches!(stmt, Stmt::Pass(_)) {
        return None;
    }
    let (start, end) = (
        stmt.range().start().to_usize(),
        stmt.range().end().to_usize(),
    );
    let (cut_start, cut_end) = line_bounds(text, start, end);
    let replacement = match (sole, cut_start == start) {
        (false, _) => String::new(),
        (true, true) => "pass".to_string(),
        (true, false) => format!("{}pass\n", &text[cut_start..start]),
    };
    Some(Cut {
        start: cut_start,
        end: cut_end,
        replacement,
        statement: true,
    })
}

fn parameter_cuts(text: &str, function: &ruff_python_ast::StmtFunctionDef, out: &mut Vec<Cut>) {
    for parameter in function.parameters.iter() {
        if matches!(parameter.name().as_str(), "self" | "cls") {
            continue;
        }
        let mut start = parameter.range().start().to_usize();
        let mut end = parameter.range().end().to_usize();
        let rest = &text[end..];
        if rest.trim_start().starts_with(',') {
            let comma = end + (rest.len() - rest.trim_start().len());
            end = comma + 1;
            end += text[end..].len() - text[end..].trim_start_matches([' ', '\t']).len();
        } else if text[..start].trim_end().ends_with(',') {
            // The last parameter takes the comma before it, not the one after.
            start = text[..start].trim_end().len() - 1;
        }
        let (cut_start, cut_end) = line_bounds(text, start, end);
        out.push(Cut {
            start: cut_start,
            end: cut_end,
            replacement: String::new(),
            statement: false,
        });
    }
}

// Whether an import binds a name the rest of `text` still uses; dropping it would leave a
// snippet that refers to something undefined.
fn import_still_used(text: &str, stmt: &Stmt) -> bool {
    let bound: Vec<&str> = match stmt {
        Stmt::Import(i) => i
            .names
            .iter()
            .map(|a| {
                a.asname
                    .as_ref()
                    .map_or_else(|| a.name.split('.').next().unwrap_or(""), |n| n.as_str())
            })
            .collect(),
        Stmt::ImportFrom(i) => i
            .names
            .iter()
            .map(|a| a.asname.as_ref().map_or(a.name.as_str(), |n| n.as_str()))
            .collect(),
        _ => return false,
    };
    let (start, end) = (
        stmt.range().start().to_usize(),
        stmt.range().end().to_usize(),
    );
    let rest = format!("{}{}", &text[..start], &text[end..]);
    bound.iter().any(|name| {
        rest.match_indices(name).any(|(at, _)| {
            let before = rest[..at].chars().next_back();
            let after = rest[at + name.len()..].chars().next();
            let is_word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
            !is_word(before) && !is_word(after)
        })
    })
}

fn collect(text: &str, suite: &[Stmt], site: usize, out: &mut Vec<Cut>) {
    for stmt in suite {
        if let Stmt::FunctionDef(function) = stmt {
            parameter_cuts(text, function, out);
        }
        let range = stmt.range();
        let holds_site = range.start().to_usize() <= site && site < range.end().to_usize();
        if !holds_site && !import_still_used(text, stmt) {
            out.extend(statement_cut(text, stmt, suite.len() == 1));
        }
        for child in child_suites(stmt) {
            collect(text, child, site, out);
        }
    }
}

fn position_of(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..offset];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, col)
}

// Collapses runs of blank lines (one inside an indented block, two between top-level
// code) and returns the new line of the one at `line`.
fn tidy(text: &str, line: usize) -> (String, usize) {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let mut new_line = line;
    let mut index = 0;
    while index < lines.len() {
        if !lines[index].trim().is_empty() {
            out.push(lines[index]);
            index += 1;
            continue;
        }
        let run_end = lines[index..]
            .iter()
            .position(|l| !l.trim().is_empty())
            .map_or(lines.len(), |p| index + p);
        let indented_next = lines
            .get(run_end)
            .is_some_and(|l| l.starts_with([' ', '\t']));
        let after_opener = out.last().is_some_and(|l| l.trim_end().ends_with(':'));
        let keep = match (out.is_empty(), run_end == lines.len(), indented_next) {
            (true, _, _) | (_, true, _) => 0,
            _ if after_opener => 0,
            (_, _, true) => 1,
            _ => 2,
        }
        .min(run_end - index);
        if line > run_end {
            new_line -= run_end - index - keep;
        }
        out.extend(std::iter::repeat_n("", keep));
        index = run_end;
    }
    let mut tidied = out.join("\n");
    tidied.push('\n');
    (tidied, new_line)
}

// `text` with the ascending, non-overlapping `cuts` made, and where `site` ends up.
fn apply(text: &str, site: usize, cuts: &[Cut]) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    let mut new_site = site;
    for cut in cuts {
        out.push_str(&text[at..cut.start]);
        out.push_str(&cut.replacement);
        at = cut.end;
        if cut.end <= site {
            new_site = new_site + cut.replacement.len() - (cut.end - cut.start);
        }
    }
    out.push_str(&text[at..]);
    (out, new_site)
}

// Deletes whole groups of statements at once: everything outermost first, then halves,
// down to pairs, so a scope that is mostly irrelevant needs a handful of checks rather
// than one per statement. What is left is for the one-at-a-time pass.
fn coarse(text: &mut String, site: &mut usize, keeps: &impl Fn(&str, (usize, usize)) -> bool) {
    let mut size = usize::MAX;
    loop {
        let Ok(parsed) = parse_module(text) else {
            return;
        };
        let mut cuts: Vec<Cut> = Vec::new();
        collect(text, &parsed.syntax().body, *site, &mut cuts);
        cuts.retain(|c| c.statement);
        cuts.sort_by_key(|c| c.start);
        let mut outer: Vec<Cut> = Vec::new();
        let mut last_end = 0;
        for cut in cuts {
            if cut.start >= last_end {
                last_end = cut.end;
                outer.push(cut);
            }
        }
        if outer.len() < 2 {
            return;
        }
        size = size.min(outer.len());
        let accepted = outer.chunks(size).find_map(|chunk| {
            let (trial, trial_site) = apply(text, *site, chunk);
            keeps(&trial, position_of(&trial, trial_site)).then_some((trial, trial_site))
        });
        match accepted {
            Some((trial, trial_site)) => {
                *text = trial;
                *site = trial_site;
            }
            None if size <= 2 => return,
            None => size = (size / 2).max(2),
        }
    }
}

/// `text` with every statement and parameter not needed to keep `keeps` satisfied removed,
/// and where the site (given as a 1-indexed line and character column) is in the result.
pub(crate) fn minimize(
    text: &str,
    position: (usize, usize),
    keeps: impl Fn(&str, (usize, usize)) -> bool,
) -> (String, (usize, usize)) {
    let index = LineIndex::from_source_text(text);
    let Some(mut site) = phrase::offset_of(text, &index, position.0, position.1) else {
        return (text.to_string(), position);
    };
    let mut text = text.to_string();
    coarse(&mut text, &mut site, &keeps);
    for _ in 0..MAX_PASSES {
        let before = text.clone();
        let mut cursor = usize::MAX;
        loop {
            let Ok(parsed) = parse_module(&text) else {
                break;
            };
            let mut cuts: Vec<Cut> = Vec::new();
            collect(&text, &parsed.syntax().body, site, &mut cuts);
            let Some(cut) = cuts
                .into_iter()
                .filter(|c| c.start < cursor)
                .max_by_key(|c| c.start)
            else {
                break;
            };
            cursor = cut.start;
            let mut trial = String::with_capacity(text.len());
            trial.push_str(&text[..cut.start]);
            trial.push_str(&cut.replacement);
            trial.push_str(&text[cut.end..]);
            let trial_site = if cut.end <= site {
                site - (cut.end - cut.start) + cut.replacement.len()
            } else {
                site
            };
            if keeps(&trial, position_of(&trial, trial_site)) {
                text = trial;
                site = trial_site;
            }
        }
        if text == before {
            break;
        }
    }
    let (line, col) = position_of(&text, site);
    let (tidied, line) = tidy(&text, line);
    (tidied, (line, col))
}
