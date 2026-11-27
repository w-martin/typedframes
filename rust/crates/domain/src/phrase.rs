//! Short, speakable ids for untyped sites, like `quiet-amber`.
//!
//! The id is a hash of the file's project-relative path, the site's own statement (its
//! tokens, so comments, spacing and line shifts don't matter) and an ordinal for identical
//! statements in one file, mapped onto two words from a fixed list. It changes when that
//! statement does, or the file moves. The word list is part of the format: editing
//! `words.txt` renames every site.

use ruff_python_parser::parse_module;
use ruff_source_file::{LineIndex, OneIndexed};
use ruff_text_size::{Ranged, TextSize};
use std::collections::HashMap;
use std::sync::OnceLock;

const WORD_BITS: u32 = 11;
const WORD_COUNT: usize = 1 << WORD_BITS;

fn words() -> &'static [&'static str] {
    static WORDS: OnceLock<Vec<&'static str>> = OnceLock::new();
    WORDS.get_or_init(|| include_str!("words.txt").lines().collect())
}

// FNV-1a over explicit bytes, so the result doesn't depend on the standard library's
// `Hash` implementations.
fn fnv(parts: &[&[u8]]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for byte in *part {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash = (hash ^ 0x1f).wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

// FNV's low bits are weak on short inputs; spread them before slicing out indices.
fn mix(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

fn phrase_of(hash: u64) -> String {
    let mixed = mix(hash);
    let mask = (WORD_COUNT - 1) as u64;
    let list = words();
    let first = list[(mixed & mask) as usize];
    let second = list[((mixed >> WORD_BITS) & mask) as usize];
    format!("{first}-{second}")
}

// The byte offset of a 1-indexed line and character column.
pub(crate) fn offset_of(source: &str, index: &LineIndex, line: usize, col: usize) -> Option<usize> {
    let mut start = index.line_start(OneIndexed::new(line)?, source).to_usize();
    // The linter's columns don't count a byte-order mark.
    if line == 1 && source.starts_with('\u{feff}') {
        start += '\u{feff}'.len_utf8();
    }
    let text = source.get(start..)?;
    let skip = col.checked_sub(1)?;
    let within = text.char_indices().nth(skip).map_or(text.len(), |(i, _)| i);
    Some(start + within)
}

/// One phrase per site, in the order given. A site is `(line, column, variable)`; `salt`
/// is the file's project-relative path.
pub(crate) fn site_phrases(
    source: &str,
    index: &LineIndex,
    salt: &str,
    sites: &[(usize, usize, &str)],
) -> Vec<String> {
    let parsed = parse_module(source).ok();
    // The parser doesn't export its token kind type, so kinds are matched by name.
    let tokens: Vec<(String, TextSize, TextSize)> = parsed.as_ref().map_or_else(Vec::new, |p| {
        p.tokens()
            .iter()
            .map(|t| {
                (
                    format!("{:?}", t.kind()),
                    t.range().start(),
                    t.range().end(),
                )
            })
            .collect()
    });

    // The text of the logical line starting at `offset`: its tokens up to the newline
    // that ends the statement, without comments or layout.
    let statement_text = |offset: usize| -> String {
        let first = tokens.partition_point(|(_, start, _)| start.to_usize() < offset);
        let mut text = String::new();
        for (kind, start, end) in &tokens[first..] {
            match kind.as_str() {
                "Newline" | "EndOfFile" => break,
                "Comment" | "NonLogicalNewline" | "Indent" | "Dedent" => {}
                _ => {
                    text.push_str(&source[start.to_usize()..end.to_usize()]);
                    text.push('\u{1f}');
                }
            }
        }
        text
    };

    // Ordinals are handed out in source order, whatever order the caller lists sites in.
    let mut order: Vec<usize> = (0..sites.len()).collect();
    order.sort_by_key(|&i| (sites[i].0, sites[i].1));
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut phrases = vec![String::new(); sites.len()];
    for i in order {
        let (line, col, var) = sites[i];
        let text = offset_of(source, index, line, col).map_or_else(String::new, &statement_text);
        let key = format!("{salt}\u{1f}{var}\u{1f}{text}");
        let ordinal = seen.entry(key.clone()).or_insert(0);
        let hash = fnv(&[key.as_bytes(), ordinal.to_string().as_bytes()]);
        *ordinal += 1;
        phrases[i] = phrase_of(hash);
    }
    phrases
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phrases(source: &str, sites: &[(usize, usize, &str)]) -> Vec<String> {
        site_phrases(
            source,
            &LineIndex::from_source_text(source),
            "job.py",
            sites,
        )
    }

    #[test]
    fn test_should_have_exactly_2048_distinct_lowercase_words() {
        let list = words();
        assert_eq!(list.len(), WORD_COUNT);
        let distinct: std::collections::HashSet<_> = list.iter().collect();
        assert_eq!(distinct.len(), WORD_COUNT);
        assert!(list
            .iter()
            .all(|w| (4..=7).contains(&w.len()) && w.chars().all(|c| c.is_ascii_lowercase())));
    }

    #[test]
    fn test_should_name_a_site_with_two_words() {
        let result = phrases("df = load()\n", &[(1, 1, "df")]);
        assert_eq!(result.len(), 1);
        let parts: Vec<_> = result[0].split('-').collect();
        assert_eq!(parts.len(), 2);
        assert!(parts.iter().all(|p| words().contains(p)));
    }

    #[test]
    fn test_should_give_the_same_phrase_on_every_call() {
        let source = "x = 1\ndf = pd.read_csv(\"a.csv\")\n";
        assert_eq!(
            phrases(source, &[(2, 1, "df")]),
            phrases(source, &[(2, 1, "df")])
        );
    }

    #[test]
    fn test_should_name_the_same_statement_differently_in_different_files() {
        let source = "df = pd.read_csv(\"a.csv\")\n";
        let index = LineIndex::from_source_text(source);
        let a = site_phrases(source, &index, "a.py", &[(1, 1, "df")]);
        let b = site_phrases(source, &index, "pkg/b.py", &[(1, 1, "df")]);
        assert_ne!(a, b);
    }

    #[test]
    fn test_should_not_change_when_lines_shift_or_layout_and_comments_change() {
        let plain = phrases("df = pd.read_csv(\"a.csv\", sep=\";\")\n", &[(1, 1, "df")]);
        let moved = phrases(
            "import os\n\n# loads the data\ndf   =  pd.read_csv(\n    \"a.csv\",  # path\n    sep=\";\"\n)\n",
            &[(4, 1, "df")],
        );
        assert_eq!(plain, moved);
    }

    #[test]
    fn test_should_change_when_the_statement_or_variable_changes() {
        let base = phrases("df = pd.read_csv(\"a.csv\")\n", &[(1, 1, "df")]);
        let other_file = phrases("df = pd.read_csv(\"b.csv\")\n", &[(1, 1, "df")]);
        let other_var = phrases("data = pd.read_csv(\"a.csv\")\n", &[(1, 1, "data")]);
        assert_ne!(base, other_file);
        assert_ne!(base, other_var);
    }

    #[test]
    fn test_should_tell_identical_statements_in_one_file_apart() {
        let source = "def a():\n    df = load()\n\n\ndef b():\n    df = load()\n";
        let result = phrases(source, &[(2, 5, "df"), (6, 5, "df")]);
        assert_ne!(result[0], result[1]);
    }

    #[test]
    fn test_should_keep_a_sites_phrase_when_an_identical_one_is_added_after_it() {
        let before = "def a():\n    df = load()\n";
        let after = "def a():\n    df = load()\n\n\ndef b():\n    df = load()\n";
        assert_eq!(
            phrases(before, &[(2, 5, "df")])[0],
            phrases(after, &[(2, 5, "df"), (6, 5, "df")])[0]
        );
    }

    #[test]
    fn test_should_return_phrases_in_the_order_the_sites_were_given() {
        let source = "a = load(1)\nb = load(2)\n";
        let forward = phrases(source, &[(1, 1, "a"), (2, 1, "b")]);
        let reverse = phrases(source, &[(2, 1, "b"), (1, 1, "a")]);
        assert_eq!(forward[0], reverse[1]);
        assert_eq!(forward[1], reverse[0]);
    }

    #[test]
    fn test_should_still_name_a_site_in_source_that_does_not_parse() {
        let result = phrases("df = (\n", &[(1, 1, "df")]);
        assert_eq!(result.len(), 1);
        assert!(!result[0].is_empty());
    }

    #[test]
    fn test_should_pin_a_known_phrase() {
        // Guards the hash, the mixing and the word list together: if this changes,
        // every site's phrase changed.
        let result = phrases("df = pd.read_csv(\"a.csv\")\n", &[(1, 1, "df")]);
        assert_eq!(result[0], "piccolo-thimble");
    }
}
