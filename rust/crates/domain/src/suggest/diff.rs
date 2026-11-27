use super::*;

pub(super) const MAX_CELLS: usize = 4_000_000;

// A shortest edit script between `old` and `new` (Myers), or `None` once it takes more than
// `MAX_EDITS` insertions and deletions, where the table would get large.
fn shortest_edit_script<'a>(old: &[&'a str], new: &[&'a str]) -> Option<Vec<(char, &'a str)>> {
    let (n, m) = (old.len() as isize, new.len() as isize);
    let limit = (n + m).min(MAX_EDITS);
    let offset = limit + 1;
    let at = |k: isize| (k + offset) as usize;
    let mut v = vec![0isize; (2 * limit + 3) as usize];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let mut found = None;
    'search: for d in 0..=limit {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let mut x = if k == -d || (k != d && v[at(k - 1)] < v[at(k + 1)]) {
                v[at(k + 1)]
            } else {
                v[at(k - 1)] + 1
            };
            let mut y = x - k;
            while x < n && y < m && old[x as usize] == new[y as usize] {
                x += 1;
                y += 1;
            }
            v[at(k)] = x;
            if x >= n && y >= m {
                found = Some(d);
                break 'search;
            }
            k += 2;
        }
    }
    let last = found?;
    let mut ops = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (0..=last).rev() {
        let before = &trace[d as usize];
        let k = x - y;
        let prev_k = if k == -d || (k != d && before[at(k - 1)] < before[at(k + 1)]) {
            k + 1
        } else {
            k - 1
        };
        let (prev_x, prev_y) = (before[at(prev_k)], before[at(prev_k)] - prev_k);
        while x > prev_x && y > prev_y {
            ops.push((' ', old[(x - 1) as usize]));
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            if x == prev_x {
                ops.push(('+', new[(y - 1) as usize]));
            } else {
                ops.push(('-', old[(x - 1) as usize]));
            }
        }
        x = prev_x;
        y = prev_y;
    }
    ops.reverse();
    Some(ops)
}

const MAX_EDITS: isize = 1000;

pub(crate) fn render_unified_diff(path: &str, old: &str, new: &str) -> String {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    let mut prefix = 0;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }
    if prefix == old_lines.len() && prefix == new_lines.len() {
        return String::new();
    }
    let mut suffix = 0;
    while suffix < old_lines.len() - prefix
        && suffix < new_lines.len() - prefix
        && old_lines[old_lines.len() - 1 - suffix] == new_lines[new_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let old_changed_end = old_lines.len() - suffix;
    let new_changed_end = new_lines.len() - suffix;
    let middle_old = &old_lines[prefix..old_changed_end];
    let middle_new = &new_lines[prefix..new_changed_end];
    let middle = shortest_edit_script(middle_old, middle_new)
        .unwrap_or_else(|| interleave(middle_old, middle_new));

    let mut ops: Vec<(char, &str)> = old_lines[..prefix].iter().map(|l| (' ', *l)).collect();
    ops.extend(middle);
    ops.extend(old_lines[old_changed_end..].iter().map(|l| (' ', *l)));

    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, (marker, _))| *marker != ' ')
        .map(|(i, _)| i)
        .collect();
    // Changes closer together than two contexts share a hunk.
    let mut hunks: Vec<(usize, usize)> = Vec::new();
    for &i in &changed {
        let from = i.saturating_sub(CONTEXT);
        let to = (i + CONTEXT + 1).min(ops.len());
        match hunks.last_mut() {
            Some(last) if from <= last.1 => last.1 = to,
            _ => hunks.push((from, to)),
        }
    }

    let mut out = format!("--- a/{path}\n+++ b/{path}\n");
    for (from, to) in hunks {
        let old_start = ops[..from].iter().filter(|(m, _)| *m != '+').count() + 1;
        let new_start = ops[..from].iter().filter(|(m, _)| *m != '-').count() + 1;
        let old_len = ops[from..to].iter().filter(|(m, _)| *m != '+').count();
        let new_len = ops[from..to].iter().filter(|(m, _)| *m != '-').count();
        out.push_str(&format!(
            "@@ -{old_start},{old_len} +{new_start},{new_len} @@\n"
        ));
        for (marker, line) in &ops[from..to] {
            out.push(*marker);
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

const CONTEXT: usize = 3;
