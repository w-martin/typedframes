//! Levenshtein edit distance and "did you mean?" suggestion lookup.
//!
//! Pure algorithm: nothing here touches the AST or linter state.

// Compute the Levenshtein edit distance between two strings using the Wagner–Fischer
// dynamic-programming algorithm.  matrix[i][j] = minimum single-character edits
// (insert, delete, substitute) to transform a[..i] into b[..j].
// Time: O(|a| × |b|).  Space: O(|a| × |b|).
// Ref: Wagner & Fischer (1974), doi:10.1145/321796.321811
pub(crate) fn levenshtein(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let a_len = a_chars.len();
    let b_len = b_chars.len();
    let mut matrix = vec![vec![0; b_len + 1]; a_len + 1];

    for (i, row) in matrix.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in matrix[0].iter_mut().enumerate() {
        *cell = j;
    }

    for i in 1..=a_len {
        for j in 1..=b_len {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            matrix[i][j] = std::cmp::min(
                std::cmp::min(matrix[i - 1][j] + 1, matrix[i][j - 1] + 1),
                matrix[i - 1][j - 1] + cost,
            );
        }
    }
    matrix[a_len][b_len]
}

// Find the closest candidate to `name` within Levenshtein distance ≤ 2.
// The threshold catches common typos (transposed letters, off-by-one characters) while
// avoiding spurious "did you mean?" hints for completely unrelated names.
pub(crate) fn find_best_match<'a>(name: &str, candidates: &'a [String]) -> Option<&'a str> {
    candidates
        .iter()
        .map(|c| (c, levenshtein(name, c)))
        // Two names that differ in most of their characters are unrelated, however short.
        .filter(|(c, dist)| *dist <= 2 && dist * 2 <= name.chars().count().max(c.chars().count()))
        .min_by_key(|(_, dist)| *dist)
        .map(|(c, _)| c.as_str())
}

// Levenshtein distance that also counts swapping two adjacent characters as one edit
// (optimal string alignment).
#[cfg(test)]
pub(crate) fn osa_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    osa_chars(&a, &b, &mut Vec::new())
}

// `osa_distance` over three rolling rows kept in `rows`, so repeated calls allocate nothing.
fn osa_chars(a: &[char], b: &[char], rows: &mut Vec<usize>) -> usize {
    let width = b.len() + 1;
    rows.clear();
    rows.resize(3 * width, 0);
    let (mut two_up, mut up, mut cur) = (0, width, 2 * width);
    for (j, cell) in rows[up..up + width].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        rows[cur] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (rows[up + j] + 1)
                .min(rows[cur + j - 1] + 1)
                .min(rows[up + j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(rows[two_up + j - 2] + 1);
            }
            rows[cur + j] = best;
        }
        (two_up, up, cur) = (up, cur, two_up);
    }
    rows[up + b.len()]
}

fn name_tokens(name: &str) -> impl Iterator<Item = String> + '_ {
    name.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
}

// Longer names are sentences or SQL, not column names a slip was made from.
const MAX_EVIDENCE_CHARS: usize = 64;

/// The names a likely-typo read is compared with: identifier-like ones only (no whitespace,
/// at most `MAX_EVIDENCE_CHARS` characters), ordered by length so only names within the
/// edit limit's length difference are ever compared.
pub(crate) struct Evidence {
    names: Vec<(Vec<char>, String)>,
    tokens: std::collections::HashMap<String, usize>,
    rows: Vec<usize>,
}

impl Evidence {
    pub(crate) fn new<I: IntoIterator<Item = String>>(names: I) -> Self {
        let mut names: Vec<(Vec<char>, String)> = names
            .into_iter()
            .filter(|n| is_identifier_like(n))
            .map(|n| (n.chars().collect(), n))
            .collect();
        names.sort_by(|a, b| a.0.len().cmp(&b.0.len()).then_with(|| a.1.cmp(&b.1)));
        names.dedup_by(|a, b| a.1 == b.1);
        let mut tokens = std::collections::HashMap::new();
        for (_, name) in &names {
            for token in name_tokens(name) {
                *tokens.entry(token).or_insert(0) += 1;
            }
        }
        Evidence {
            names,
            tokens,
            rows: Vec::new(),
        }
    }

    /// The one name `name` is most likely a misspelling of: within two edits (one for a name
    /// under five characters), with no other evidence name as close, and with a word of
    /// `name` that no other evidence name has -- a slip makes a new word, while `arr_hour`
    /// next to `dep_hour` and `arr_delay` is a composition of known words. `work` is charged
    /// for every character pair compared; `None` once it would run out.
    pub(crate) fn typo(&mut self, name: &str, work: &mut usize) -> Option<String> {
        if !is_identifier_like(name) {
            return None;
        }
        let ours_chars: Vec<char> = name.chars().collect();
        let len = ours_chars.len();
        let limit = if len < 5 { 1 } else { 2 };
        let lo = self.names.partition_point(|(c, _)| c.len() + limit < len);
        let hi = self.names.partition_point(|(c, _)| c.len() <= len + limit);
        let mut nearest: Option<(usize, usize, bool)> = None;
        for index in lo..hi {
            let (chars, candidate) = &self.names[index];
            if candidate == name {
                continue;
            }
            let cost = (len + 1) * (chars.len() + 1);
            let Some(left) = work.checked_sub(cost) else {
                *work = 0;
                return None;
            };
            *work = left;
            let d = osa_chars(&ours_chars, chars, &mut self.rows);
            if d > limit {
                continue;
            }
            nearest = match nearest {
                Some((best, at, _)) if d == best => Some((best, at, true)),
                Some((best, _, _)) if d > best => nearest,
                _ => Some((d, index, false)),
            };
        }
        let (_, at, tied) = nearest?;
        if tied {
            return None;
        }
        let candidate = self.names[at].1.as_str();
        // `d_precip` beside `o_precip`, `col_1` beside `col_2`: a sibling, not a slip.
        let ours: Vec<String> = name_tokens(name).collect();
        let theirs: Vec<String> = name_tokens(candidate).collect();
        // `cat_x` beside `cat`: one name extends the other by whole words.
        let (short, long) = if ours.len() < theirs.len() {
            (&ours, &theirs)
        } else {
            (&theirs, &ours)
        };
        if short.len() < long.len() && (long.starts_with(short) || long.ends_with(short)) {
            return None;
        }
        if ours.len() == theirs.len() {
            let differing: Vec<(&String, &String)> =
                ours.iter().zip(&theirs).filter(|(a, b)| a != b).collect();
            if let [(a, b)] = differing.as_slice() {
                if a.chars().count() <= 2 && b.chars().count() <= 2 {
                    return None;
                }
            }
        }
        // Tokens of the other evidence names: `name` itself, when it is evidence, doesn't count.
        let own: Vec<String> = if self.names.iter().any(|(_, n)| n == name) {
            name_tokens(name).collect()
        } else {
            Vec::new()
        };
        let known = |t: &String| {
            let mine = own.iter().filter(|o| *o == t).count();
            self.tokens.get(t).copied().unwrap_or(0) > mine
        };
        ours.iter()
            .any(|t| !known(t))
            .then(|| candidate.to_string())
    }
}

fn is_identifier_like(name: &str) -> bool {
    !name.is_empty()
        && !name.chars().any(char::is_whitespace)
        && name.chars().count() <= MAX_EVIDENCE_CHARS
}

#[cfg(test)]
pub(crate) fn evidence_typo(name: &str, evidence: &[String]) -> Option<String> {
    Evidence::new(evidence.iter().cloned()).typo(name, &mut usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_count_an_adjacent_swap_as_one_edit() {
        assert_eq!(osa_distance("year_buitl", "year_built"), 1);
        assert_eq!(osa_distance("abc", "abc"), 0);
        assert_eq!(osa_distance("ticketid", "ticket_id"), 1);
    }

    #[test]
    fn test_should_name_the_single_close_evidence_name_of_a_new_word() {
        // arrange
        let evidence: Vec<String> = ["year_built", "seats", "tail_num", "dep_hour", "arr_delay"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        // act / assert
        assert_eq!(
            evidence_typo("year_buitl", &evidence).as_deref(),
            Some("year_built")
        );
        assert_eq!(evidence_typo("arr_hour", &evidence), None);
        assert_eq!(evidence_typo("sets", &evidence).as_deref(), Some("seats"));
        assert_eq!(evidence_typo("totally_new", &evidence), None);
    }

    #[test]
    fn test_should_only_compare_identifier_like_names_of_a_close_length() {
        // arrange
        let sentence = "the quick brown fox jumps over the lazy dog".to_string();
        let long = "x".repeat(65);
        let mut evidence = Evidence::new([
            sentence.clone(),
            long.clone(),
            "seats".into(),
            "a".repeat(40),
        ]);
        let mut work = usize::MAX;

        // act
        let found = evidence.typo("sets", &mut work);

        // assert
        assert_eq!(found.as_deref(), Some("seats"));
        assert_eq!(
            usize::MAX - work,
            5 * 6,
            "only 'seats' is within the length window"
        );
        assert_eq!(evidence.typo(&sentence, &mut work), None);
        let mut tight = 10;
        assert_eq!(evidence.typo("sets", &mut tight), None);
        assert_eq!(tight, 0);
    }

    #[test]
    fn test_should_not_pick_between_two_equally_close_names() {
        // arrange
        let evidence: Vec<String> = ["o_precip", "d_precip"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        // act / assert
        assert_eq!(evidence_typo("x_precip", &evidence), None);
        assert_eq!(
            evidence_typo("o_prcip", &evidence).as_deref(),
            Some("o_precip")
        );
        assert_eq!(evidence_typo("q_precip", &["o_precip".to_string()]), None);
        assert_eq!(evidence_typo("cat_x", &["cat".to_string()]), None);
    }

    #[test]
    fn test_should_not_suggest_a_name_that_shares_nothing_with_a_short_one() {
        // arrange
        let candidates = vec!["d".to_string(), "m".to_string()];

        // act / assert
        assert_eq!(find_best_match("zz", &candidates), None);
        assert_eq!(find_best_match("bb", &["b".to_string()]), Some("b"));
    }

    #[test]
    fn test_should_compute_levenshtein_distance() {
        // arrange
        let a = "email";
        let b = "emai";

        // act
        let dist = levenshtein(a, b);

        // assert
        assert_eq!(dist, 1);
    }

    #[test]
    fn test_should_find_best_match_for_typo() {
        // arrange
        let name = "emai";
        let candidates = vec!["user_id".to_string(), "email".to_string()];

        // act
        let result = find_best_match(name, &candidates);

        // assert
        assert_eq!(result, Some("email"));
    }

    #[test]
    fn test_levenshtein() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("flaw", "lawn"), 2);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("equal", "equal"), 0);
    }
}
