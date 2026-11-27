use super::*;

pub(super) struct Tok {
    pub(super) kind: String,
    pub(super) start: usize,
    pub(super) end: usize,
}

/// Renames as it goes, remembering what it called each original.
#[derive(Default)]
pub(super) struct Namer {
    // Names a kept import brought in, which stay wherever they are used.
    pub(super) imported: std::collections::HashSet<String>,
    pub(super) names: HashMap<String, String>,
    pub(super) strings: HashMap<String, String>,
    pub(super) counts: HashMap<char, usize>,
}

impl Namer {
    // `library`: whether a library name may stay here; false where the code is defining the
    // name, or a capitalised name is used as a plain value.
    pub(super) fn name(&mut self, original: &str, prefix: char, library: bool) -> String {
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
        // The checker treats a `test_` function as a test, and the prefix says nothing else.
        let renamed = if original.starts_with("test_") {
            format!("test_{prefix}{count}")
        } else {
            format!("{prefix}{count}")
        };
        self.names.insert(original.to_string(), renamed.clone());
        renamed
    }

    pub(super) fn string(&mut self, token_text: &str, keyword: Option<&str>) -> String {
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

pub(super) struct Redactor<'a> {
    pub(super) source: &'a str,
    pub(super) tokens: &'a [Tok],
    pub(super) namer: Namer,
    pub(super) out: String,
    pub(super) site_offset: usize,
    pub(super) site_position: Option<(usize, usize)>,
    pub(super) refusal: Option<String>,
    // Redacting a kept import, whose names are the library's own.
    pub(super) in_import: bool,
}

// Token kinds that are syntax rather than data: copied as written.
const VERBATIM_KINDS: &[&str] = &[
    "Newline",
    "NonLogicalNewline",
    "Indent",
    "Dedent",
    "EndOfFile",
    "FStringStart",
    "FStringEnd",
    "TStringStart",
    "TStringEnd",
    "Exclamation",
    "Lpar",
    "Rpar",
    "Lsqb",
    "Rsqb",
    "Colon",
    "Comma",
    "Semi",
    "Plus",
    "Minus",
    "Star",
    "Slash",
    "Vbar",
    "Amper",
    "Less",
    "Greater",
    "Equal",
    "Dot",
    "Percent",
    "Lbrace",
    "Rbrace",
    "EqEqual",
    "NotEqual",
    "LessEqual",
    "GreaterEqual",
    "Tilde",
    "CircumFlex",
    "LeftShift",
    "RightShift",
    "DoubleStar",
    "DoubleStarEqual",
    "PlusEqual",
    "MinusEqual",
    "StarEqual",
    "SlashEqual",
    "PercentEqual",
    "AmperEqual",
    "VbarEqual",
    "CircumflexEqual",
    "LeftShiftEqual",
    "RightShiftEqual",
    "DoubleSlash",
    "DoubleSlashEqual",
    "ColonEqual",
    "At",
    "AtEqual",
    "Rarrow",
    "Ellipsis",
    "And",
    "As",
    "Assert",
    "Async",
    "Await",
    "Break",
    "Class",
    "Continue",
    "Def",
    "Del",
    "Elif",
    "Else",
    "Except",
    "False",
    "Finally",
    "For",
    "From",
    "Global",
    "If",
    "Import",
    "In",
    "Is",
    "Lambda",
    "None",
    "Nonlocal",
    "Not",
    "Or",
    "Pass",
    "Raise",
    "Return",
    "True",
    "Try",
    "While",
    "With",
    "Yield",
    "Case",
    "Match",
    "Type",
];

pub(super) fn copies_verbatim(kind: &str) -> bool {
    VERBATIM_KINDS.contains(&kind)
}

// Numbers are often data (ids, thresholds, amounts): only 0 and 1 are kept.
pub(super) fn masked_number(kind: &str, text: &str) -> String {
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

    pub(super) fn here(&self) -> (usize, usize) {
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
    pub(super) fn redact(&mut self, start: usize, end: usize) {
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
            self.redact_token(floor, ceil, &mut depth, index, kind, text);
        }
    }

    fn redact_token(
        &mut self,
        floor: usize,
        ceil: usize,
        depth: &mut i32,
        index: usize,
        kind: String,
        text: &str,
    ) {
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
            "Name" => self.redact_name_token(floor, ceil, depth, index, text),
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
            "Lpar" | "Lsqb" | "Lbrace" => *depth += 1,
            "Rpar" | "Rsqb" | "Rbrace" => *depth -= 1,
            _ => {}
        }
        self.out.push_str(&piece);
    }

    fn redact_name_token(
        &mut self,
        floor: usize,
        ceil: usize,
        depth: &mut i32,
        index: usize,
        text: &str,
    ) -> String {
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
        } else if *depth > 0 && after == "Equal" {
            'k'
        } else {
            'v'
        };
        // A bare name is the library's only if an import said so.
        let library = before == "Dot" || prefix == 'k';
        self.namer.name(text, prefix, library)
    }
}

pub(super) fn contains(range: ruff_text_size::TextRange, offset: usize) -> bool {
    range.start().to_usize() <= offset && offset < range.end().to_usize()
}

// Every name in `start..end`.
pub(super) fn names_in(source: &str, tokens: &[Tok], start: usize, end: usize) -> BTreeSet<String> {
    let first = tokens.partition_point(|t| t.start < start);
    let last = tokens.partition_point(|t| t.start < end);
    tokens[first..last]
        .iter()
        .filter(|t| t.kind == "Name")
        .map(|t| source[t.start..t.end].to_string())
        .collect()
}

// Names called anywhere in `start..end`: a name directly followed by `(`.
pub(super) fn callee_names(
    source: &str,
    tokens: &[Tok],
    start: usize,
    end: usize,
) -> BTreeSet<String> {
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

pub(super) fn is_kept_import(stmt: &Stmt) -> bool {
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

// A class's header and the listed members, redacted into `redactor`'s output.
pub(super) fn redact_class(
    redactor: &mut Redactor,
    source: &str,
    tokens: &[Tok],
    stmt: &Stmt,
    class: &StmtClassDef,
    members: &BTreeSet<usize>,
) {
    redactor.out.push_str("\n\n");
    let keyword = tokens
        .iter()
        .find(|t| t.start >= stmt.range().start().to_usize() && t.kind == "Class")
        .map_or(stmt.range().start().to_usize(), |t| t.start);
    let header_end = end_of_logical_line(tokens, keyword);
    redactor.redact(keyword, header_end);
    for &m in members {
        let member = &class.body[m];
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
        if m != *members.iter().next_back().unwrap_or(&m) {
            redactor.out.push('\n');
        }
    }
}
