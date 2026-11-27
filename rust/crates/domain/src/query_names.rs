//! The column names a pandas `query`/`eval` expression string reads and assigns.

/// Names that are never columns in a pandas expression: keywords, constants pandas resolves
/// itself, and the index resolvers.
const NOT_COLUMNS: &[&str] = &[
    "and",
    "or",
    "not",
    "in",
    "is",
    "if",
    "else",
    "lambda",
    "for",
    "True",
    "False",
    "None",
    "inf",
    "Inf",
    "nan",
    "NaN",
    "index",
    "Timestamp",
    "datetime",
    "list",
    "tuple",
];

#[derive(Debug, Default, PartialEq)]
pub(crate) struct QueryNames {
    pub reads: Vec<String>,
    pub assigned: Vec<String>,
}

/// Scan `text` for bare identifiers and backticked names. `@locals`, attributes after a `.`,
/// function names (followed by `(`), string literals and numbers are skipped. `None` when the
/// text cannot be scanned with confidence (an unterminated quote or backtick).
pub(crate) fn query_names(text: &str) -> Option<QueryNames> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = QueryNames::default();
    let mut i = 0;
    let next_non_space = |from: usize| chars[from..].iter().find(|c| !c.is_whitespace()).copied();
    let assigns_at = |from: usize| {
        let rest: Vec<char> = chars[from..]
            .iter()
            .skip_while(|c| c.is_whitespace())
            .take(2)
            .copied()
            .collect();
        rest.first() == Some(&'=') && rest.get(1) != Some(&'=')
    };
    let push = |name: String, end: usize, out: &mut QueryNames| {
        if assigns_at(end) {
            out.assigned.push(name);
        } else if !out.reads.contains(&name) {
            out.reads.push(name);
        }
    };
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' || c == '"' {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != c {
                if chars[j] == '\\' {
                    j += 1;
                }
                j += 1;
            }
            if j >= chars.len() {
                return None;
            }
            i = j + 1;
        } else if c == '`' {
            let end = i + 1 + chars[i + 1..].iter().position(|&ch| ch == '`')?;
            let name: String = chars[i + 1..end].iter().collect();
            push(name, end + 1, &mut out);
            i = end + 1;
        } else if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let name: String = chars[start..i].iter().collect();
            let before = chars[..start].iter().rev().find(|c| !c.is_whitespace());
            let skipped = matches!(before, Some('@' | '.'))
                || next_non_space(i) == Some('(')
                || NOT_COLUMNS.contains(&name.as_str())
                || name.starts_with("ilevel_")
                || name.starts_with("clevel_");
            if !skipped {
                push(name, i, &mut out);
            }
        } else if c.is_ascii_digit() {
            while i < chars.len() && (chars[i].is_alphanumeric() || matches!(chars[i], '_' | '.')) {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    out.reads.retain(|r| !out.assigned.contains(r));
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reads(text: &str) -> Vec<String> {
        query_names(text).unwrap().reads
    }

    #[test]
    fn test_should_read_bare_and_backticked_names() {
        assert_eq!(reads("a > 3 and `b c` < 2"), ["a", "b c"]);
    }

    #[test]
    fn test_should_skip_locals_attributes_functions_strings_and_numbers() {
        assert_eq!(
            reads("a > @lim and b.str.contains('x y') and abs(c) > 1.5e3 and d in [\"q\"]"),
            ["a", "b", "c", "d"]
        );
    }

    #[test]
    fn test_should_skip_keywords_and_index_resolvers() {
        assert_eq!(reads("not a and index > 2 or ilevel_0 == True"), ["a"]);
    }

    #[test]
    fn test_should_separate_assigned_names_from_reads() {
        let names = query_names("c = a + b\nd = c * 2").unwrap();
        assert_eq!(names.assigned, ["c", "d"]);
        assert_eq!(names.reads, ["a", "b"]);
    }

    #[test]
    fn test_should_not_treat_equality_as_assignment() {
        assert_eq!(reads("a == 1"), ["a"]);
    }

    #[test]
    fn test_should_give_up_on_an_unterminated_quote() {
        assert_eq!(query_names("a == 'x"), None);
    }
}
