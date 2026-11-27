use super::*;

// The same read can be reached by the assignment dispatch and by the argument checks. The one
// that suggests a spelling says the most.
pub(super) fn keep_most_helpful_unknown_column(errors: &mut Vec<LintError>) {
    let mut best: HashMap<(usize, String, String), usize> = HashMap::new();
    for (index, e) in errors.iter().enumerate() {
        if e.code != CODE_UNKNOWN_COLUMN {
            best.insert((index, String::new(), String::new()), index);
            continue;
        }
        let stem = e.message.split(" (did you mean").next().unwrap_or_default();
        let key = (e.line, e.code.clone(), stem.to_string());
        match best.get(&key) {
            Some(&kept) if errors[kept].message.len() >= e.message.len() => {}
            _ => {
                best.insert(key, index);
            }
        }
    }
    let keep: std::collections::HashSet<usize> = best.into_values().collect();
    let mut index = 0;
    errors.retain(|_| {
        index += 1;
        keep.contains(&(index - 1))
    });
}

// The column an unknown-column error names, and the inferred column set it names.
fn column_and_set(e: &LintError) -> Option<(String, Vec<String>)> {
    let rest = e.message.strip_prefix("Column '")?;
    let (column, rest) = rest.split_once("' does not exist in inferred column set {")?;
    let (set, _) = rest.split_once('}')?;
    Some((
        column.to_string(),
        set.split(", ").map(str::to_string).collect(),
    ))
}

fn error_rank(e: &LintError) -> (bool, usize) {
    (e.message.contains("(did you mean"), e.col)
}

// One frame seen before and after the statement that reads it (the post-statement set only adds
// that statement's outputs): one mistake, so one error. Operands with unrelated column sets (both
// sides of a merge) still report separately. Which errors to drop, by position.
pub(super) fn repeats_of_nested_column_sets(errors: &[LintError]) -> Vec<bool> {
    let mut dropped = vec![false; errors.len()];
    // Compared only within a line, so a file with very many errors stays linear.
    let parsed: Vec<Option<(String, Vec<String>)>> = errors.iter().map(column_and_set).collect();
    let mut by_line: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, e) in errors.iter().enumerate() {
        if parsed[i].is_some() {
            by_line.entry(e.line).or_default().push(i);
        }
    }
    for same_line in by_line.values() {
        for &i in same_line {
            for &j in same_line {
                if repeats(errors, &parsed, &dropped, i, j) {
                    dropped[i] = true;
                }
            }
        }
    }
    dropped
}

// Whether error `i` repeats error `j`: the same column, nested column sets, and `j` says more.
pub(super) fn repeats(
    errors: &[LintError],
    parsed: &[Option<(String, Vec<String>)>],
    dropped: &[bool],
    i: usize,
    j: usize,
) -> bool {
    if i == j || dropped[i] || dropped[j] {
        return false;
    }
    let (Some((ci, si)), Some((cj, sj))) = (&parsed[i], &parsed[j]) else {
        return false;
    };
    let nested = si.iter().all(|c| sj.contains(c)) || sj.iter().all(|c| si.contains(c));
    ci == cj && nested && error_rank(&errors[i]) < error_rank(&errors[j])
}
