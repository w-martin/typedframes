//! Bug-report material for one untyped site: how the checker classified it, and a
//! redacted copy of the code around it that is classified the same way.
//!
//! The snippet is the source's own tokens with every name and string renamed, except
//! those listed in `report_names` (library and builtin names, option strings). Only the
//! code the checker looked at is kept: the enclosing function or class member, same-file
//! functions it calls, library imports, and a signature-only stub for an imported
//! function whose library the index knew. The redacted code is then run through the
//! checker; if it isn't classified exactly as the real site was, no snippet is given.

use crate::linter::Linter;
use crate::phrase;
use crate::report_minimize::minimize;
use crate::report_names::{keeps_library_name, keeps_name, keeps_string, KEPT_IMPORT_ROOTS};
use crate::suggest::Library;
use ruff_python_ast::{Stmt, StmtClassDef};
use ruff_python_parser::parse_module;
use ruff_source_file::LineIndex;
use ruff_text_size::Ranged;
use serde::Serialize;
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

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

// Replaces quoted names, backticked names and numbers with placeholders.
fn mask(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut previous = ' ';
    while let Some(c) = chars.next() {
        let opens_quote = c == '\'' && !previous.is_alphanumeric();
        if c == '`' || opens_quote {
            let close = c;
            let mut closed = false;
            let mut inner = String::new();
            for next in chars.by_ref() {
                if next == close {
                    closed = true;
                    break;
                }
                inner.push(next);
            }
            if closed {
                out.push(close);
                out.push('_');
                out.push(close);
            } else {
                out.push(c);
                out.push_str(&inner);
            }
            previous = close;
        } else if c.is_ascii_digit() {
            while chars.peek().is_some_and(char::is_ascii_digit) {
                chars.next();
            }
            out.push('N');
            previous = 'N';
        } else {
            out.push(c);
            previous = c;
        }
    }
    out
}

// Masks `text`, first hiding the site's own column names: they are the one thing a
// reason quotes that can itself contain a quote or backtick, which `mask` can't pair up.
fn mask_hiding(text: &str, columns: &[String]) -> String {
    let mut ordered: Vec<&String> = columns.iter().collect();
    ordered.sort_by_key(|c| std::cmp::Reverse(c.len()));
    let mut hidden = text.to_string();
    for column in ordered {
        hidden = hidden.replace(&format!("'{column}'"), "'_'");
    }
    mask(&hidden)
}

fn verdict_of(linter: &Linter, phrase: &str) -> Option<Verdict> {
    if let Some(s) = linter.suggestions.iter().find(|s| s.phrase == phrase) {
        return Some(Verdict {
            outcome: "suggested",
            kind: Some(s.kind.to_string()),
            tier: Some(s.tier.to_string()),
            reason: mask_hiding(&s.reason, &s.columns),
            notes: s.notes.iter().map(|n| mask_hiding(n, &s.columns)).collect(),
        });
    }
    linter
        .withheld
        .iter()
        .find(|w| w.phrase == phrase)
        .map(|w| Verdict {
            outcome: "withheld",
            kind: None,
            tier: None,
            reason: mask(&w.reason),
            notes: Vec::new(),
        })
}

fn analyse(source: &str) -> Result<Linter, String> {
    let mut linter = Linter::new();
    linter.collect_suggestions = true;
    linter.suggest_display_path = "snippet.py".to_string();
    linter
        .check_file_internal(source, Path::new("snippet.py"))
        .map_err(|_| "the snippet could not be checked".to_string())?;
    Ok(linter)
}

// A reason without how many more uses there were: dropping code can lower that count
// without changing why the checker decided as it did.
fn without_count(reason: &str) -> &str {
    reason.strip_suffix(", and N more").unwrap_or(reason)
}

fn same_verdict(a: &Verdict, b: &Verdict) -> bool {
    a.outcome == b.outcome
        && a.kind == b.kind
        && a.tier == b.tier
        && without_count(&a.reason) == without_count(&b.reason)
}

/// Runs the snippet through the checker and confirms the site at `position` gets the
/// same verdict the real one did.
fn verify(snippet: &str, position: (usize, usize), real: &Verdict) -> Result<(), String> {
    let linter = analyse(snippet)?;
    let site = linter
        .untyped_sites
        .iter()
        .find(|s| (s.line, s.col) == position)
        .ok_or("the snippet has no untyped site where the original was")?;
    let redacted = verdict_of(&linter, &site.phrase)
        .ok_or("the snippet's site was neither suggested nor withheld")?;
    if same_verdict(&redacted, real) {
        Ok(())
    } else {
        Err(format!(
            "the snippet is classified differently ({} here, {} originally)",
            redacted.outcome, real.outcome
        ))
    }
}

struct Tok {
    kind: String,
    start: usize,
    end: usize,
}

/// Renames as it goes, remembering what it called each original.
#[derive(Default)]
struct Namer {
    // Names a kept import brought in, which stay wherever they are used.
    imported: std::collections::HashSet<String>,
    names: HashMap<String, String>,
    strings: HashMap<String, String>,
    counts: HashMap<char, usize>,
}

impl Namer {
    // `library`: whether a library name may stay here; false where the code is defining the
    // name, or a capitalised name is used as a plain value.
    fn name(&mut self, original: &str, prefix: char, library: bool) -> String {
        if keeps_name(original)
            || self.imported.contains(original)
            || (library && keeps_library_name(original, prefix))
        {
            return original.to_string();
        }
        if let Some(found) = self.names.get(original) {
            return found.clone();
        }
        let count = self.counts.entry(prefix).or_insert(0);
        *count += 1;
        let renamed = format!("{prefix}{count}");
        self.names.insert(original.to_string(), renamed.clone());
        renamed
    }

    fn string(&mut self, token_text: &str, keyword: Option<&str>) -> String {
        let inner = string_inner(token_text);
        if inner.is_empty() || keeps_string(inner, keyword) {
            return token_text.to_string();
        }
        let next = self.strings.len() + 1;
        let renamed = self
            .strings
            .entry(inner.to_string())
            .or_insert_with(|| format!("c{next}"));
        format!("\"{renamed}\"")
    }
}

// The text between a string literal's quotes, without any prefix.
fn string_inner(text: &str) -> &str {
    let body = text.trim_start_matches(|c: char| c.is_ascii_alphabetic());
    let quote = if body.starts_with("\"\"\"") || body.starts_with("'''") {
        3
    } else {
        1
    };
    if body.len() >= 2 * quote {
        &body[quote..body.len() - quote]
    } else {
        ""
    }
}

struct Redactor<'a> {
    source: &'a str,
    tokens: &'a [Tok],
    namer: Namer,
    out: String,
    site_offset: usize,
    site_position: Option<(usize, usize)>,
    refusal: Option<String>,
    // Redacting a kept import, whose names are the library's own.
    in_import: bool,
}

// Token kinds that are syntax rather than data: copied as written.
fn copies_verbatim(kind: &str) -> bool {
    matches!(
        kind,
        "Newline"
            | "NonLogicalNewline"
            | "Indent"
            | "Dedent"
            | "EndOfFile"
            | "FStringStart"
            | "FStringEnd"
            | "TStringStart"
            | "TStringEnd"
            | "Exclamation"
            | "Lpar"
            | "Rpar"
            | "Lsqb"
            | "Rsqb"
            | "Colon"
            | "Comma"
            | "Semi"
            | "Plus"
            | "Minus"
            | "Star"
            | "Slash"
            | "Vbar"
            | "Amper"
            | "Less"
            | "Greater"
            | "Equal"
            | "Dot"
            | "Percent"
            | "Lbrace"
            | "Rbrace"
            | "EqEqual"
            | "NotEqual"
            | "LessEqual"
            | "GreaterEqual"
            | "Tilde"
            | "CircumFlex"
            | "LeftShift"
            | "RightShift"
            | "DoubleStar"
            | "DoubleStarEqual"
            | "PlusEqual"
            | "MinusEqual"
            | "StarEqual"
            | "SlashEqual"
            | "PercentEqual"
            | "AmperEqual"
            | "VbarEqual"
            | "CircumflexEqual"
            | "LeftShiftEqual"
            | "RightShiftEqual"
            | "DoubleSlash"
            | "DoubleSlashEqual"
            | "ColonEqual"
            | "At"
            | "AtEqual"
            | "Rarrow"
            | "Ellipsis"
            | "And"
            | "As"
            | "Assert"
            | "Async"
            | "Await"
            | "Break"
            | "Class"
            | "Continue"
            | "Def"
            | "Del"
            | "Elif"
            | "Else"
            | "Except"
            | "False"
            | "Finally"
            | "For"
            | "From"
            | "Global"
            | "If"
            | "Import"
            | "In"
            | "Is"
            | "Lambda"
            | "None"
            | "Nonlocal"
            | "Not"
            | "Or"
            | "Pass"
            | "Raise"
            | "Return"
            | "True"
            | "Try"
            | "While"
            | "With"
            | "Yield"
            | "Case"
            | "Match"
            | "Type"
    )
}

// Numbers are often data (ids, thresholds, amounts): only 0 and 1 are kept.
fn masked_number(kind: &str, text: &str) -> String {
    match (kind, text) {
        ("Int", "0" | "1") | ("Float", "0.0" | "1.0") => text.to_string(),
        ("Float", _) => "2.5".to_string(),
        ("Complex", _) => "2j".to_string(),
        _ => "2".to_string(),
    }
}

impl Redactor<'_> {
    // The keyword a string at `index` is the value of (`how` in `how="left"`), if any.
    fn keyword_before(&self, index: usize, floor: usize) -> Option<String> {
        let mut earlier = self.tokens[floor..index]
            .iter()
            .rev()
            .filter(|t| !Self::is_trivia(&t.kind));
        let equals = earlier.next()?;
        let name = earlier.next()?;
        (equals.kind == "Equal" && name.kind == "Name")
            .then(|| self.source[name.start..name.end].to_string())
    }

    fn token_range(&self, start: usize, end: usize) -> std::ops::Range<usize> {
        let first = self.tokens.partition_point(|t| t.start < start);
        let last = self.tokens.partition_point(|t| t.start < end);
        first..last
    }

    fn is_trivia(kind: &str) -> bool {
        matches!(kind, "Comment" | "NonLogicalNewline")
    }

    fn previous_kind(&self, from: usize, floor: usize) -> &str {
        self.tokens[floor..from]
            .iter()
            .rev()
            .find(|t| !Self::is_trivia(&t.kind))
            .map_or("", |t| t.kind.as_str())
    }

    fn next_kind(&self, from: usize, ceil: usize) -> &str {
        self.tokens[from + 1..ceil]
            .iter()
            .find(|t| !Self::is_trivia(&t.kind))
            .map_or("", |t| t.kind.as_str())
    }

    fn here(&self) -> (usize, usize) {
        let line = self.out.matches('\n').count() + 1;
        let col = self
            .out
            .rsplit('\n')
            .next()
            .map_or(0, |l| l.chars().count())
            + 1;
        (line, col)
    }

    // Appends the redacted tokens of `start..end`, keeping the layout between them.
    fn redact(&mut self, start: usize, end: usize) {
        let range = self.token_range(start, end);
        let (floor, ceil) = (range.start, range.end);
        let mut depth: i32 = 0;
        let mut previous_end: Option<usize> = None;
        let mut dropped_comment_line = false;
        for index in range {
            let (kind, tok_start, tok_end) = {
                let t = &self.tokens[index];
                (t.kind.clone(), t.start, t.end)
            };
            if let Some(end_of_previous) = previous_end {
                self.out
                    .push_str(&self.source[end_of_previous..tok_start.max(end_of_previous)]);
            }
            previous_end = Some(tok_end);
            if tok_start == self.site_offset && self.site_position.is_none() {
                self.site_position = Some(self.here());
            }
            let text = &self.source[tok_start..tok_end];
            if kind == "Comment" {
                // A comment alone on its line takes the whole line with it.
                let line_start = self.out.rfind('\n').map_or(0, |p| p + 1);
                dropped_comment_line = self.out[line_start..].trim().is_empty();
                if dropped_comment_line {
                    self.out.truncate(line_start);
                }
                continue;
            }
            if dropped_comment_line && kind == "NonLogicalNewline" {
                dropped_comment_line = false;
                continue;
            }
            dropped_comment_line = false;
            let piece = match kind.as_str() {
                "String" => {
                    let keyword = self.keyword_before(index, floor);
                    self.namer.string(text, keyword.as_deref())
                }
                "FStringMiddle" | "TStringMiddle" => "s".to_string(),
                "Int" | "Float" | "Complex" => masked_number(&kind, text),
                // `!r`, `!s` and `!a` are syntax, not names.
                "Name" if self.previous_kind(index, floor) == "Exclamation" => text.to_string(),
                "Name" if self.in_import => {
                    self.namer.imported.insert(text.to_string());
                    text.to_string()
                }
                "Name" => {
                    let before = self.previous_kind(index, floor);
                    let after = self.next_kind(index, ceil);
                    let prefix = if before == "Def" {
                        'f'
                    } else if before == "Class" {
                        'C'
                    } else if before == "Dot" || after == "Lpar" {
                        if before == "Dot" && after != "Lpar" {
                            'a'
                        } else {
                            'f'
                        }
                    } else if depth > 0 && after == "Equal" {
                        'k'
                    } else {
                        'v'
                    };
                    // A bare name is the library's only if an import said so.
                    let library = before == "Dot" || prefix == 'k';
                    self.namer.name(text, prefix, library)
                }
                other if copies_verbatim(other) => text.to_string(),
                other => {
                    // Anything not known to be syntax could carry data, so nothing is shown.
                    self.refusal.get_or_insert_with(|| {
                        format!("it contains a token the redactor does not handle ({other})")
                    });
                    String::new()
                }
            };
            match kind.as_str() {
                "Lpar" | "Lsqb" | "Lbrace" => depth += 1,
                "Rpar" | "Rsqb" | "Rbrace" => depth -= 1,
                _ => {}
            }
            self.out.push_str(&piece);
        }
    }
}

fn contains(range: ruff_text_size::TextRange, offset: usize) -> bool {
    range.start().to_usize() <= offset && offset < range.end().to_usize()
}

// Names called anywhere in `start..end`: a name directly followed by `(`.
fn callee_names(source: &str, tokens: &[Tok], start: usize, end: usize) -> BTreeSet<String> {
    let first = tokens.partition_point(|t| t.start < start);
    let last = tokens.partition_point(|t| t.start < end);
    let mut names = BTreeSet::new();
    for pair in tokens[first..last].windows(2) {
        if pair[0].kind == "Name" && pair[1].kind == "Lpar" {
            names.insert(source[pair[0].start..pair[0].end].to_string());
        }
    }
    names
}

fn is_kept_import(stmt: &Stmt) -> bool {
    let root_kept =
        |name: &str| KEPT_IMPORT_ROOTS.contains(&name.split('.').next().unwrap_or(name));
    match stmt {
        Stmt::Import(i) => i.names.iter().any(|a| root_kept(a.name.as_str())),
        Stmt::ImportFrom(i) => {
            i.level == 0 && i.module.as_ref().is_some_and(|m| root_kept(m.as_str()))
        }
        _ => false,
    }
}

// The offset just after the first newline token at or after `start`.
fn end_of_logical_line(tokens: &[Tok], start: usize) -> usize {
    let first = tokens.partition_point(|t| t.start < start);
    tokens[first..]
        .iter()
        .find(|t| t.kind == "Newline")
        .map_or(start, |t| t.end)
}

/// Redacted code around the site at `site_offset`, and where the site ended up in it.
#[allow(clippy::too_many_lines)]
fn build_snippet(
    source: &str,
    site_offset: usize,
    stubs: &HashMap<String, Library>,
) -> Result<(String, (usize, usize)), String> {
    let parsed = parse_module(source).map_err(|_| "the file does not parse".to_string())?;
    let tokens: Vec<Tok> = parsed
        .tokens()
        .iter()
        .map(|t| Tok {
            kind: format!("{:?}", t.kind()),
            start: t.range().start().to_usize(),
            end: t.range().end().to_usize(),
        })
        .collect();
    let body = &parsed.syntax().body;
    let top = body
        .iter()
        .position(|s| contains(s.range(), site_offset))
        .ok_or("the site is not inside any statement")?;

    // What to keep: top-level statements (functions, or module-level code), plus the
    // members of the site's class when it is in one.
    let mut tops: BTreeSet<usize> = BTreeSet::new();
    let mut class_members: BTreeSet<usize> = BTreeSet::new();
    let mut class_top: Option<usize> = None;
    match &body[top] {
        Stmt::FunctionDef(_) => {
            tops.insert(top);
        }
        Stmt::ClassDef(class) => {
            let member = class
                .body
                .iter()
                .position(|s| contains(s.range(), site_offset))
                .ok_or("the site is not inside a class member")?;
            class_top = Some(top);
            class_members.insert(member);
        }
        _ => {
            for (i, stmt) in body.iter().enumerate() {
                if !matches!(
                    stmt,
                    Stmt::FunctionDef(_)
                        | Stmt::ClassDef(_)
                        | Stmt::Import(_)
                        | Stmt::ImportFrom(_)
                ) {
                    tops.insert(i);
                }
            }
        }
    }
    let class: Option<&StmtClassDef> = class_top.and_then(|i| match &body[i] {
        Stmt::ClassDef(c) => Some(c),
        _ => None,
    });

    // Pull in same-file functions and class members the kept code calls.
    let mut added = 0;
    loop {
        let mut wanted: BTreeSet<String> = BTreeSet::new();
        for &i in &tops {
            let r = body[i].range();
            wanted.extend(callee_names(
                source,
                &tokens,
                r.start().to_usize(),
                r.end().to_usize(),
            ));
        }
        if let Some(c) = class {
            for &m in &class_members {
                let r = c.body[m].range();
                wanted.extend(callee_names(
                    source,
                    &tokens,
                    r.start().to_usize(),
                    r.end().to_usize(),
                ));
            }
        }
        let mut grew = false;
        for (i, stmt) in body.iter().enumerate() {
            if let Stmt::FunctionDef(f) = stmt {
                if added < MAX_HELPERS && !tops.contains(&i) && wanted.contains(f.name.as_str()) {
                    tops.insert(i);
                    added += 1;
                    grew = true;
                }
            }
        }
        if let Some(c) = class {
            for (m, stmt) in c.body.iter().enumerate() {
                if let Stmt::FunctionDef(f) = stmt {
                    if added < MAX_HELPERS
                        && !class_members.contains(&m)
                        && wanted.contains(f.name.as_str())
                    {
                        class_members.insert(m);
                        added += 1;
                        grew = true;
                    }
                }
            }
        }
        if !grew {
            break;
        }
    }

    let mut redactor = Redactor {
        source,
        tokens: &tokens,
        namer: Namer::default(),
        out: String::new(),
        site_offset,
        site_position: None,
        refusal: None,
        in_import: false,
    };

    // Library imports.
    redactor.in_import = true;
    for stmt in body.iter().filter(|s| is_kept_import(s)) {
        redactor.redact(
            stmt.range().start().to_usize(),
            stmt.range().end().to_usize(),
        );
        redactor.out.push('\n');
    }
    redactor.in_import = false;

    // Signature-only stand-ins for imported functions whose library the index knew.
    let defined: BTreeSet<&str> = body
        .iter()
        .filter_map(|s| match s {
            Stmt::FunctionDef(f) => Some(f.name.as_str()),
            _ => None,
        })
        .collect();
    let mut called: BTreeSet<String> = BTreeSet::new();
    for &i in &tops {
        let r = body[i].range();
        called.extend(callee_names(
            source,
            &tokens,
            r.start().to_usize(),
            r.end().to_usize(),
        ));
    }
    if let Some(c) = class {
        for &m in &class_members {
            let r = c.body[m].range();
            called.extend(callee_names(
                source,
                &tokens,
                r.start().to_usize(),
                r.end().to_usize(),
            ));
        }
    }
    let mut stub_lines = String::new();
    let mut stub_libraries: BTreeSet<&'static str> = BTreeSet::new();
    for name in &called {
        if defined.contains(name.as_str()) {
            continue;
        }
        if let Some(&library) = stubs.get(name) {
            let (alias, module) = match library {
                Library::Pandas => ("pd", "pandas"),
                Library::Polars => ("pl", "polars"),
            };
            stub_libraries.insert(module);
            let renamed = redactor.namer.name(name, 'f', false);
            stub_lines.push_str(&format!(
                "def {renamed}(*args, **kwargs) -> {alias}.DataFrame: ...\n"
            ));
        }
    }
    for module in stub_libraries {
        let alias = if module == "pandas" { "pd" } else { "pl" };
        let line = format!("import {module} as {alias}\n");
        if !redactor.out.contains(&line) {
            redactor.out.insert_str(0, &line);
        }
    }
    // The inserted imports shift the site's recorded position only if it was already
    // seen; imports come first, so nothing has been recorded yet.
    redactor.out.push_str(&stub_lines);

    // The kept code, in source order.
    for (i, stmt) in body.iter().enumerate() {
        if tops.contains(&i) {
            redactor.out.push_str("\n\n");
            redactor.redact(
                stmt.range().start().to_usize(),
                stmt.range().end().to_usize(),
            );
            redactor.out.push('\n');
        } else if class_top == Some(i) {
            let c = class.expect("class_top implies a class");
            redactor.out.push_str("\n\n");
            let keyword = tokens
                .iter()
                .find(|t| t.start >= stmt.range().start().to_usize() && t.kind == "Class")
                .map_or(stmt.range().start().to_usize(), |t| t.start);
            let header_end = end_of_logical_line(&tokens, keyword);
            redactor.redact(keyword, header_end);
            for &m in &class_members {
                let member = &c.body[m];
                let start = member.range().start().to_usize();
                let line_start = source[..start].rfind('\n').map_or(0, |p| p + 1);
                let indent = &source[line_start..start];
                redactor.out.push_str(if indent.trim().is_empty() {
                    indent
                } else {
                    "    "
                });
                redactor.redact(start, member.range().end().to_usize());
                redactor.out.push('\n');
                if m != *class_members.iter().next_back().unwrap_or(&m) {
                    redactor.out.push('\n');
                }
            }
        }
    }

    if let Some(reason) = redactor.refusal.take() {
        return Err(reason);
    }
    let position = redactor
        .site_position
        .ok_or("the site is not inside the code that was kept")?;
    let text: String = redactor
        .out
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    let trimmed = text.trim_start_matches('\n');
    let removed_lines = text.len() - trimmed.len();
    let snippet = format!("{trimmed}\n");
    // Leading blank lines were removed, so the recorded line moves up by that many.
    if snippet.lines().count() > MAX_UNREDUCED_LINES {
        return Err(format!(
            "the code around the site is {} lines, over the {MAX_UNREDUCED_LINES}-line limit; trim it by hand",
            snippet.lines().count()
        ));
    }
    Ok((snippet, (position.0 - removed_lines, position.1)))
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
    let site = linter.untyped_sites.iter().find(|s| s.phrase == name)?;
    let verdict = verdict_of(linter, name)?;
    let index = LineIndex::from_source_text(source);
    let offset = phrase::offset_of(source, &index, site.line, site.col);
    let built = offset
        .ok_or_else(|| "the site's position could not be found".to_string())
        .and_then(|offset| build_snippet(source, offset, linter.imported_function_libraries()))
        .and_then(|(snippet, position)| {
            verify(&snippet, position, &verdict)?;
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
mod tests {
    use super::*;

    fn linter_for(source: &str) -> Linter {
        let mut linter = Linter::new();
        linter.collect_suggestions = true;
        linter.suggest_display_path = "job.py".to_string();
        linter
            .check_file_internal(source, Path::new("job.py"))
            .unwrap();
        linter
    }

    fn report(source: &str, var: &str) -> SiteReport {
        let linter = linter_for(source);
        let name = linter
            .untyped_sites
            .iter()
            .find(|s| s.var == var)
            .expect("no such site")
            .phrase
            .clone();
        report_for(source, &linter, &name).expect("site has a report")
    }

    // The redacted code before it is reduced, for tests of what redaction keeps and hides.
    fn unreduced(source: &str, var: &str) -> String {
        let linter = linter_for(source);
        let site = linter
            .untyped_sites
            .iter()
            .find(|s| s.var == var)
            .expect("no such site");
        let index = LineIndex::from_source_text(source);
        let offset = phrase::offset_of(source, &index, site.line, site.col).expect("offset");
        build_snippet(source, offset, linter.imported_function_libraries())
            .expect("a snippet")
            .0
    }

    const PRIVATE: &str = r#"import pandas as pd
from acme_internal import secret_loader


class PricingService:
    def run(self, customer_id):
        revenue = pd.read_csv("s3://acme/customers.csv", sep=";")
        total = revenue["gross_margin"].sum() + revenue["loss"].sum()
        self.audit(revenue)
        return total + customer_id

    def audit(self, frame):
        # TODO: ask about the quarterly numbers
        return frame.groupby("region").sum()
"#;

    #[test]
    fn test_should_keep_none_of_the_original_names_or_strings() {
        // act
        let snippet = unreduced(PRIVATE, "revenue");

        // assert
        for secret in [
            "acme",
            "PricingService",
            "customer",
            "revenue",
            "gross_margin",
            "loss",
            "region",
            "quarterly",
            "audit",
            "secret_loader",
            "s3://",
            "TODO",
        ] {
            assert!(
                !snippet.contains(secret),
                "{secret} leaked into:\n{snippet}"
            );
        }
        assert!(snippet.contains("pd.read_csv("), "{snippet}");
        assert!(snippet.contains("sep=\"c"), "{snippet}");
        assert!(snippet.contains(".groupby("), "{snippet}");
    }

    #[test]
    fn test_should_keep_library_names_where_the_library_uses_them_and_rename_ordinary_variables() {
        // arrange
        let source = "import numpy as np\nimport pandas as pd\n\n\ndef run(tenants: np.ndarray, value):\n    raw = pd.read_csv(\"x.csv\")\n    value = np.percentile(tenants, 2, axis=0)\n    return raw.fillna(value=value), raw.shape\n";

        // act
        let snippet = unreduced(source, "raw");

        // assert
        assert!(snippet.contains("np.ndarray"), "{snippet}");
        assert!(snippet.contains("np.percentile("), "{snippet}");
        assert!(snippet.contains(".fillna(value="), "{snippet}");
        assert!(snippet.contains(".shape"), "{snippet}");
        assert!(!snippet.contains("tenants"), "{snippet}");
        assert!(snippet.contains("v2 = np.percentile("), "{snippet}");
        assert!(!snippet.contains("value ="), "{snippet}");
    }

    #[test]
    fn test_should_rename_user_definitions_and_values_that_share_a_library_name() {
        // arrange: `load`, `Config` and `Catalog` are numpy or polars names too.
        let source = "import pandas as pd\n\n\nclass Catalog:\n    pass\n\n\ndef load(path):\n    return pd.read_csv(path)\n\n\ndef run():\n    Config = Catalog\n    raw = pd.read_csv(\"x.csv\")\n    print(load(raw))\n    return raw[\"a\"], Config\n";

        // act
        let snippet = unreduced(source, "raw");

        // assert
        for secret in ["Catalog", "Config", "load"] {
            assert!(
                !snippet.contains(secret),
                "{secret} leaked into:\n{snippet}"
            );
        }
        assert!(snippet.contains("pd.read_csv("), "{snippet}");
    }

    #[test]
    fn test_should_keep_a_name_imported_from_a_library_wherever_it_is_used() {
        // arrange
        let source = "from pandas import DataFrame\nimport pandas as pd\n\n\ndef run(rows) -> DataFrame:\n    raw = pd.read_csv(\"x.csv\")\n    return DataFrame(raw[\"a\"])\n";

        // act
        let snippet = unreduced(source, "raw");

        // assert
        assert!(
            snippet.contains("from pandas import DataFrame"),
            "{snippet}"
        );
        assert!(snippet.contains("-> DataFrame:"), "{snippet}");
        assert!(snippet.contains("return DataFrame("), "{snippet}");
    }

    #[test]
    fn test_should_not_leak_through_fstrings_docstrings_decorators_or_bases() {
        // arrange
        let source = r#"import pandas as pd
import acme_internal


@acme_internal.register("acme-jobs")
class NightlyJob(acme_internal.BaseJob):
    """Loads the acme pricing numbers."""

    def run(self, tenant):
        """Runs for one tenant."""
        path = f"/data/acme/{tenant}/orders_{self.stamp:%Y}.csv"
        orders = pd.read_csv(path)
        picked = {"acme_key": lambda row: row.acme_field}
        orders["acme_total"] = orders["acme_price"] * 2
        return orders.acme_status, picked
"#;

        // act
        let report = report(source, "orders");

        // assert
        let snippet = report.snippet.expect("a snippet was produced");
        for secret in [
            "acme", "tenant", "Nightly", "orders", "stamp", "pricing", "/data", "status",
        ] {
            assert!(
                !snippet.contains(secret),
                "{secret} leaked into:\n{snippet}"
            );
        }
    }

    #[test]
    fn test_should_rename_the_same_thing_the_same_way() {
        // act
        let snippet = unreduced(PRIVATE, "revenue");

        // assert: the frame is renamed once and used under that name
        let first_line = snippet
            .lines()
            .find(|l| l.contains("read_csv"))
            .expect("the load is kept");
        let renamed = first_line.trim().split(' ').next().unwrap().to_string();
        assert!(
            snippet.matches(&format!("{renamed}.")).count()
                + snippet.matches(&format!("{renamed}[")).count()
                >= 2,
            "{snippet}"
        );
    }

    #[test]
    fn test_should_redact_template_strings_numbers_and_f_string_conversions() {
        // arrange
        let source = r#"import pandas as pd


def run(who):
    path = f"/data/{who!r}/orders.csv"
    note = t"secret template {who} text_secret"
    limit = 12345678
    mask = 0xDEAD_BEEF
    rate = 0.0125
    raw = pd.read_csv(path)
    return raw["a"], note, limit, mask, rate, 0, 1
"#;

        // act
        let snippet = unreduced(source, "raw");

        // assert
        for secret in [
            "secret",
            "text_secret",
            "12345678",
            "DEAD",
            "0.0125",
            "orders",
            "data",
        ] {
            assert!(
                !snippet.contains(secret),
                "{secret} leaked into:\n{snippet}"
            );
        }
        assert!(snippet.contains("!r}"), "{snippet}");
        assert!(snippet.contains(", 0, 1"), "{snippet}");
    }

    #[test]
    fn test_should_not_leak_a_column_name_containing_a_quote_through_the_verdict() {
        // arrange
        let source = "import pandas as pd\n\n\ndef run():\n    raw = pd.read_json(\"x.json\")\n    print(raw[\"o'brien_secret\"], raw[\"amount\"])\n";

        // act
        let report = report(source, "raw");

        // assert
        assert_eq!(report.verdict.outcome, "suggested");
        let text = format!("{} {:?}", report.verdict.reason, report.verdict.notes);
        assert!(
            !text.contains("brien") && !text.contains("secret"),
            "{text}"
        );
        assert!(report.snippet.is_some(), "{:?}", report.refusal);
    }

    #[test]
    fn test_should_keep_option_strings_only_as_the_value_of_an_option_keyword() {
        // arrange
        let source = r#"import pandas as pd


def run(other):
    raw = pd.read_csv("x.csv")
    merged = raw.merge(other, how="left")
    return merged["left"], {"count": raw["first"]}
"#;

        // act
        let snippet = report(source, "raw").snippet.expect("a snippet");

        // assert
        assert!(snippet.contains("how=\"left\""), "{snippet}");
        assert_eq!(snippet.matches("\"left\"").count(), 1, "{snippet}");
        assert!(
            !snippet.contains("\"count\"") && !snippet.contains("\"first\""),
            "{snippet}"
        );
    }

    #[test]
    fn test_should_rename_dunder_names_that_are_not_standard() {
        // arrange
        let source = "import pandas as pd\n\n\ndef run():\n    __acme_secret__ = 1\n    raw = pd.read_csv(\"x.csv\")\n    return raw[\"a\"], __acme_secret__\n";

        // act
        let snippet = report(source, "raw").snippet.expect("a snippet");

        // assert
        assert!(!snippet.contains("acme"), "{snippet}");
    }

    #[test]
    fn test_should_copy_only_syntax_tokens_verbatim() {
        for kind in [
            "Lpar",
            "Dot",
            "Equal",
            "Def",
            "Return",
            "Newline",
            "Indent",
            "FStringStart",
            "TStringEnd",
        ] {
            assert!(copies_verbatim(kind), "{kind}");
        }
        for kind in [
            "Name",
            "String",
            "Int",
            "Float",
            "Complex",
            "FStringMiddle",
            "TStringMiddle",
            "Comment",
            "IpyEscapeCommand",
            "Unknown",
            "Question",
            "SomethingNew",
        ] {
            assert!(!copies_verbatim(kind), "{kind}");
        }
    }

    #[test]
    fn test_should_keep_only_zero_and_one_as_numbers() {
        assert_eq!(masked_number("Int", "0"), "0");
        assert_eq!(masked_number("Int", "1"), "1");
        assert_eq!(masked_number("Int", "42"), "2");
        assert_eq!(masked_number("Int", "0xDEAD_BEEF"), "2");
        assert_eq!(masked_number("Float", "1.0"), "1.0");
        assert_eq!(masked_number("Float", "0.0125"), "2.5");
        assert_eq!(masked_number("Complex", "3j"), "2j");
    }

    #[test]
    fn test_should_keep_a_snippet_for_common_operators_and_keywords() {
        // arrange: if a common token kind were missing from the verbatim list this would refuse
        let source = r#"import pandas as pd


def run(items, flag):
    raw = pd.read_csv("x.csv")
    total = 0
    for item in items:
        if item is not None and flag or not flag:
            total += item ** 2 // 3 % 5 << 1 >> 1 & 1 | 0 ^ 1
    try:
        pick = [x for x in items if x >= 1 and x <= 9 and x != 3 and x == 4]
        lam = lambda y: -y @ y
        while total < 100:
            total -= 1
            break
    except ValueError as err:
        raise RuntimeError from err
    finally:
        pass
    with open("f") as handle:
        value = (yield)
    assert total > 0, "m"
    return raw["a"], pick, lam, total, handle, value, ..., ~total
"#;

        // act
        let report = report(source, "raw");

        // assert
        assert!(report.snippet.is_some(), "{:?}", report.refusal);
    }

    #[test]
    fn test_should_keep_the_helper_the_scope_calls_and_the_class_around_it() {
        // act
        let snippet = unreduced(PRIVATE, "revenue");

        // assert
        assert!(snippet.contains("class C1"), "{snippet}");
        assert_eq!(snippet.matches("def ").count(), 2, "{snippet}");
        assert!(snippet.contains("import pandas as pd"));
        assert!(!snippet.contains("import acme"));
    }

    #[test]
    fn test_should_give_a_snippet_that_is_classified_like_the_original() {
        // act
        let report = report(PRIVATE, "revenue");

        // assert
        let snippet = report.snippet.expect("snippet");
        let redacted = linter_for(&snippet);
        assert_eq!(redacted.untyped_sites.len(), 1);
        let again = verdict_of(&redacted, &redacted.untyped_sites[0].phrase).unwrap();
        assert_eq!(again, report.verdict);
        assert!(report.refusal.is_none());
    }

    #[test]
    fn test_should_report_a_withheld_site_with_a_masked_reason() {
        // arrange
        let source = "import pandas as pd\n\n\ndef run():\n    raw = pd.read_csv(\"x.csv\")\n    print(raw[\"a\"])\n    return clean_the_data(raw)\n";

        // act
        let report = report(source, "raw");

        // assert
        assert_eq!(report.verdict.outcome, "withheld");
        assert_eq!(
            report.verdict.reason,
            "can't see everything done with it: passed to `_` (line N)"
        );
        assert!(!report.verdict.reason.contains("clean_the_data"));
        assert!(report.snippet.is_some());
    }

    #[test]
    fn test_should_refuse_a_snippet_that_is_classified_differently() {
        // arrange: the withheld site's blocker removed makes it suggestible
        let source = "import pandas as pd\n\n\ndef run():\n    raw = pd.read_csv(\"x.csv\")\n    print(raw[\"a\"])\n    return clean(raw)\n";
        let real = report(source, "raw").verdict;
        let tampered = "import pandas as pd\n\n\ndef run():\n    raw = pd.read_csv(\"x.csv\")\n    print(raw[\"a\"])\n    return 1\n";

        // act
        let result = verify(tampered, (5, 5), &real);

        // assert
        let error = result.expect_err("a different verdict must be refused");
        assert!(error.contains("classified differently"), "{error}");
    }

    #[test]
    fn test_should_reduce_a_scope_too_large_to_paste_to_the_code_that_decides_the_verdict() {
        // arrange
        let mut source =
            String::from("import pandas as pd\n\n\ndef run():\n    raw = pd.read_csv(\"x.csv\")\n");
        for i in 0..200 {
            source.push_str(&format!("    print({i})\n"));
        }
        source.push_str("    return raw[\"a\"]\n");

        // act
        let report = report(&source, "raw");

        // assert
        let snippet = report.snippet.expect("a snippet once the noise is dropped");
        assert!(!snippet.contains("print"), "{snippet}");
        assert!(snippet.contains("read_csv"), "{snippet}");
        assert!(snippet.lines().count() < 10, "{snippet}");
    }

    #[test]
    fn test_should_refuse_a_site_whose_own_statement_is_too_large_to_paste() {
        // arrange
        let mut source = String::from(
            "import pandas as pd\n\n\ndef run():\n    raw = pd.read_csv(\n        \"x.csv\",\n        na_values=[\n",
        );
        for i in 0..200 {
            source.push_str(&format!("            \"n{i}\",\n"));
        }
        source.push_str("        ],\n    )\n    return raw[\"a\"]\n");

        // act
        let report = report(&source, "raw");

        // assert
        assert!(report.snippet.is_none());
        assert!(report.refusal.unwrap().contains("trim it by hand"));
    }

    #[test]
    fn test_should_drop_code_that_does_not_decide_the_verdict() {
        // act
        let snippet = report(PRIVATE, "revenue").snippet.unwrap();

        // assert: the load and the call it escapes into stay; the arithmetic does not
        assert!(snippet.contains("read_csv"), "{snippet}");
        assert!(!snippet.contains('+'), "{snippet}");
        assert!(!snippet.contains("groupby"), "{snippet}");
        assert!(!snippet.contains("sum"), "{snippet}");
    }

    #[test]
    fn test_should_keep_the_use_that_withholds_a_site_and_drop_the_rest() {
        // arrange
        let source = "import pandas as pd\n\n\ndef run(a, b):\n    raw = pd.read_csv(\"x.csv\")\n    print(raw[\"a\"])\n    return clean_the_data(raw)\n";

        // act
        let report = report(source, "raw");

        // assert
        assert_eq!(report.verdict.outcome, "withheld");
        let snippet = report.snippet.expect("snippet");
        assert!(!snippet.contains("print"), "{snippet}");
        assert_eq!(snippet.matches("(v").count(), 1, "{snippet}");
        assert!(
            snippet.contains("def f1():"),
            "unused parameters go: {snippet}"
        );
    }

    #[test]
    fn test_should_give_a_reduced_snippet_that_is_still_classified_like_the_original() {
        // act
        let report = report(PRIVATE, "revenue");

        // assert
        let snippet = report.snippet.expect("snippet");
        let redacted = linter_for(&snippet);
        assert_eq!(redacted.untyped_sites.len(), 1);
        let again = verdict_of(&redacted, &redacted.untyped_sites[0].phrase).unwrap();
        assert!(same_verdict(&again, &report.verdict));
    }

    #[test]
    fn test_should_stand_in_for_an_imported_function_by_signature_only() {
        // arrange
        let source = "from lib_private import load_orders\n\n\ndef run():\n    df = load_orders()\n    return df[\"a\"]\n";
        let offset = source.find("df =").unwrap();
        let mut stubs = HashMap::new();
        stubs.insert("load_orders".to_string(), Library::Pandas);

        // act
        let (snippet, _position) = build_snippet(source, offset, &stubs).unwrap();

        // assert
        assert!(snippet.contains("-> pd.DataFrame: ..."), "{snippet}");
        assert!(snippet.contains("import pandas as pd"), "{snippet}");
        assert!(!snippet.contains("load_orders"), "{snippet}");
        assert!(!snippet.contains("lib_private"), "{snippet}");
    }

    #[test]
    fn test_should_mask_names_quotes_and_numbers_in_a_verdict() {
        assert_eq!(
            mask("only 'amount', 'region' are read later (lines 6, 8)"),
            "only '_', '_' are read later (lines N, N)"
        );
        assert_eq!(
            mask("passed to `clean()` (line 14)"),
            "passed to `_` (line N)"
        );
        assert_eq!(
            mask("can't see everything done with it: x"),
            "can't see everything done with it: x"
        );
    }

    #[test]
    fn test_should_find_a_site_on_the_first_line_of_a_file_with_a_byte_order_mark() {
        // arrange
        let source = "\u{feff}import pandas as pd; df = pd.read_csv(\"x.csv\")\nprint(df[\"a\"])\n";
        let linter = linter_for(source);
        let site = &linter.untyped_sites[0];

        // act
        let offset = phrase::offset_of(
            source,
            &LineIndex::from_source_text(source),
            site.line,
            site.col,
        )
        .unwrap();

        // assert
        assert!(
            source[offset..].starts_with("df ="),
            "{}",
            &source[offset..]
        );
    }

    #[test]
    fn test_should_report_every_untyped_site_in_a_file() {
        // arrange
        let source = "import pandas as pd\n\n\ndef one():\n    a = pd.read_csv(\"x.csv\")\n    return a[\"x\"]\n\n\ndef two():\n    b = pd.read_csv(\"y.csv\")\n    print(b[\"y\"])\n    return keep(b)\n";
        let linter = linter_for(source);

        // act
        let reports = report_all(source, &linter);

        // assert
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].var, "a");
        assert_eq!(reports[1].var, "b");
        assert!(reports.iter().all(|r| r.snippet.is_some()));
    }

    #[test]
    fn test_should_keep_an_import_the_remaining_code_still_uses_and_drop_one_it_does_not() {
        // arrange
        let source = "import numpy as np\nimport pandas as pd\n\n\nclass S:\n    def make(self, rows) -> pd.DataFrame:\n        return pd.DataFrame(rows)\n\n    def use(self):\n        n = np.zeros(3)\n        df = self.make([])\n        return df[\"a\"], n\n";

        // act
        let snippet = report(source, "df").snippet.expect("snippet");

        // assert
        assert!(snippet.contains("import pandas as pd"), "{snippet}");
        assert!(!snippet.contains("numpy"), "{snippet}");
    }

    #[test]
    fn test_should_return_nothing_for_a_name_the_file_does_not_have() {
        // arrange
        let source = "import pandas as pd\ndf = pd.read_csv(\"x.csv\")\nprint(df[\"a\"])\n";
        let linter = linter_for(source);

        // act / assert
        assert!(report_for(source, &linter, "no-such").is_none());
    }
}
