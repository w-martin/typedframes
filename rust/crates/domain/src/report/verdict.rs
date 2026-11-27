use super::*;

// Whether backticked code in a reason may stay as written: every name in it is one a snippet
// keeps in the same position, and it holds no string or number that could be data.
fn keeps_code(code: &str) -> bool {
    let chars: Vec<char> = code.chars().collect();
    let next_solid = |from: usize| (from..chars.len()).find(|&j| !chars[j].is_whitespace());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if matches!(c, '\'' | '"') || c.is_ascii_digit() {
            return false;
        }
        if !(c.is_alphabetic() || c == '_') {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
            i += 1;
        }
        let name: String = chars[start..i].iter().collect();
        let before = chars[..start].iter().rev().find(|c| !c.is_whitespace());
        let after = next_solid(i);
        // As the snippet renames: a library member stays after a dot and a library keyword
        // before `=`, but a bare name or call (`load`, `keep()`) is taken to be the user's.
        let library = if before == Some(&'.') {
            keeps_library_name(&name, 'a')
        } else if after.is_some_and(|j| chars[j] == '=' && chars.get(j + 1) != Some(&'=')) {
            keeps_library_name(&name, 'k')
        } else {
            false
        };
        let constant = matches!(name.as_str(), "True" | "False" | "None");
        if !(constant || library || keeps_name(&name)) {
            return false;
        }
    }
    true
}

// Replaces quoted names, backticked names and numbers with placeholders. Backticked code
// whose names are all library or Python names stays, so a reason still says which method
// or call withheld the site.
pub(super) fn mask(text: &str) -> String {
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
            if closed && close == '`' && keeps_code(&inner) {
                out.push(close);
                out.push_str(&inner);
                out.push(close);
            } else if closed {
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
    count_hidden_columns(&mask(&hidden))
}

// `'_', '_', '_'` says nothing a count doesn't: "3 columns".
pub(super) fn count_hidden_columns(text: &str) -> String {
    const HIDDEN: &str = "'_'";
    const SEPARATOR: &str = ", ";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(HIDDEN) {
        out.push_str(&rest[..start]);
        let mut after = &rest[start + HIDDEN.len()..];
        let mut count = 1;
        while let Some(next) = after
            .strip_prefix(SEPARATOR)
            .and_then(|a| a.strip_prefix(HIDDEN))
        {
            after = next;
            count += 1;
        }
        if count > 1 {
            out.push_str(&format!("{count} columns"));
        } else {
            out.push_str(HIDDEN);
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

pub(super) fn verdict_of(linter: &Linter, phrase: &str) -> Option<Verdict> {
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

pub(super) fn analyse(source: &str) -> Result<Linter, String> {
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

pub(super) fn same_verdict(a: &Verdict, b: &Verdict) -> bool {
    a.outcome == b.outcome
        && a.kind == b.kind
        && a.tier == b.tier
        && without_count(&a.reason) == without_count(&b.reason)
}

/// Runs the snippet through the checker and confirms the site at `position` gets the
/// same verdict the real one did.
pub(super) fn verify(
    snippet: &str,
    position: (usize, usize),
    real: &Verdict,
) -> Result<(), String> {
    let linter = analyse(snippet)?;
    let site = linter
        .untyped_sites
        .iter()
        .find(|s| (s.line, s.col) == position)
        .ok_or(
            "the snippet has no untyped site where the original was: the site depends on code the \
             snippet does not keep (another class, an imported name, or the shape of a call), so \
             describe how the frame is built by hand",
        )?;
    let redacted = verdict_of(&linter, &site.phrase)
        .ok_or("the snippet's site was neither suggested nor withheld")?;
    if same_verdict(&redacted, real) {
        Ok(())
    } else if redacted.outcome == real.outcome {
        Err(format!(
            "the snippet is {} for another reason: {}",
            redacted.outcome, redacted.reason
        ))
    } else {
        Err(format!(
            "the snippet is classified differently ({} here, {} originally)",
            redacted.outcome, real.outcome
        ))
    }
}
