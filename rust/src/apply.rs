//! Plans every file's edits, re-checks the edited source, then writes all accepted files
//! or none. Edits are byte ranges against the analysed source, so each file is re-read
//! before writing and left alone if it changed.

use crate::config::{find_project_root, load_linter_config};
use crate::errors::LintError;
use crate::index::ProjectIndex;
use crate::pyapi::{linter_with_index, CheckError};
use crate::suggest::{Edit, Suggestion};
use ruff_python_parser::parse_module;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Which of a file's suggestions to apply.
pub(crate) enum Selection {
    /// Every suggestion whose tier is `safe`.
    SafeTier,
    /// Exactly the suggestions whose origin is at these `(line, col)` positions, whatever
    /// their tier -- what the user chose one by one.
    Origins(Vec<(usize, usize)>),
}

/// A source file's suggestions, the source they were made from, and its current errors.
pub(crate) struct Analyzed {
    pub(crate) source: String,
    pub(crate) suggestions: Vec<Suggestion>,
    // Line numbers removed, so inserting a line doesn't change a key.
    pub(crate) errors: Vec<String>,
}

fn error_keys(errors: &[LintError]) -> Vec<String> {
    let mut keys: Vec<String> = errors
        .iter()
        .filter(|e| e.severity == "error")
        .map(|e| format!("{}\u{0}{}", e.code, strip_line_numbers(&e.message)))
        .collect();
    keys.sort();
    keys
}

fn ends_with_word(text: &str, word: &str) -> bool {
    text.strip_suffix(word)
        .is_some_and(|before| !before.ends_with(|c: char| c.is_alphanumeric() || c == '_'))
}

// Drops the digits of `line 12` / `file.py:12` references, which shift on insertion.
fn strip_line_numbers(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut rest = message;
    while let Some(start) = rest.find(|c: char| c.is_ascii_digit()) {
        let (before, digits_on) = rest.split_at(start);
        out.push_str(before);
        let length = digits_on
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(digits_on.len());
        let is_line_number =
            ends_with_word(&out, "line ") || out.ends_with(".py:") || out.ends_with(".ipynb:");
        if !is_line_number {
            out.push_str(&digits_on[..length]);
        }
        rest = &digits_on[length..];
    }
    out.push_str(rest);
    out
}

/// Errors present in `after` beyond those in `before`, counting duplicates.
fn new_errors(before: &[String], after: &[String]) -> usize {
    let mut remaining: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for key in before {
        *remaining.entry(key).or_default() += 1;
    }
    after
        .iter()
        .filter(|key| match remaining.get_mut(key.as_str()) {
            Some(count) if *count > 0 => {
                *count -= 1;
                false
            }
            _ => true,
        })
        .count()
}

pub(crate) fn analyze(
    path: &Path,
    display_path: &str,
    index: Option<Arc<ProjectIndex>>,
) -> Result<Option<Analyzed>, CheckError> {
    let project_root = find_project_root(path);
    let config = load_linter_config(&project_root);
    if !config.enabled.unwrap_or(true) {
        return Ok(None);
    }
    let source = fs::read_to_string(path).map_err(|e| CheckError::Io(e.to_string()))?;
    let (mut linter, _) = linter_with_index(path, &project_root, &config, &source, index);
    linter.collect_suggestions = true;
    linter.suggest_display_path = display_path.to_string();
    let errors = linter
        .check_file_internal(&source, path)
        .map_err(|e| CheckError::Runtime(e.to_string()))?;
    Ok(Some(Analyzed {
        source,
        suggestions: std::mem::take(&mut linter.suggestions),
        errors: error_keys(&errors),
    }))
}

#[derive(Serialize, Clone, Debug)]
pub struct AppliedSuggestion {
    pub line: usize,
    pub col: usize,
    pub var: String,
    pub columns: Vec<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct SkippedSuggestion {
    pub line: usize,
    pub col: usize,
    pub reason: String,
}

#[derive(Serialize, Debug, Default)]
pub struct FileOutcome {
    pub file: String,
    pub applied: Vec<AppliedSuggestion>,
    pub skipped: Vec<SkippedSuggestion>,
    pub errors_before: usize,
    pub errors_after: usize,
    pub written: bool,
    pub error: Option<String>,
}

/// An accepted change to one file, ready to write.
pub(crate) struct Planned {
    path: PathBuf,
    original: String,
    edited: String,
}

/// `edits` applied to `source`. Edits are insertions or replacements at distinct places;
/// later offsets go first so earlier ones stay valid.
pub(crate) fn apply_edits(source: &str, edits: &[Edit]) -> Option<String> {
    let mut ordered: Vec<&Edit> = edits.iter().collect();
    ordered.sort_by(|a, b| b.start.cmp(&a.start).then(b.end.cmp(&a.end)));
    let mut out = source.to_string();
    let mut previous_start = usize::MAX;
    for edit in ordered {
        if edit.end > previous_start
            || edit.start > edit.end
            || !out.is_char_boundary(edit.start)
            || !out.is_char_boundary(edit.end)
            || edit.end > out.len()
        {
            return None;
        }
        out.replace_range(edit.start..edit.end, &edit.replacement);
        previous_start = edit.start;
    }
    Some(out)
}

/// Works out what to do for one file. Nothing is written here.
pub(crate) fn plan_file(
    path: &Path,
    display_path: &str,
    selection: &Selection,
    index: Option<Arc<ProjectIndex>>,
) -> (FileOutcome, Option<Planned>) {
    let mut outcome = FileOutcome {
        file: display_path.to_string(),
        ..FileOutcome::default()
    };
    let analyzed = match analyze(path, display_path, index.clone()) {
        Ok(Some(analyzed)) => analyzed,
        Ok(None) => return (outcome, None),
        Err(error) => {
            outcome.error = Some(match error {
                CheckError::Io(m) | CheckError::Runtime(m) => m,
            });
            return (outcome, None);
        }
    };
    outcome.errors_before = analyzed.errors.len();
    outcome.errors_after = analyzed.errors.len();

    let mut chosen: Vec<&Suggestion> = Vec::new();
    match selection {
        Selection::SafeTier => {
            chosen.extend(analyzed.suggestions.iter().filter(|s| s.tier == "safe"))
        }
        Selection::Origins(wanted) => {
            for (line, col) in wanted {
                match analyzed
                    .suggestions
                    .iter()
                    .find(|s| s.line == *line && s.col == *col)
                {
                    Some(suggestion) => chosen.push(suggestion),
                    None => outcome.skipped.push(SkippedSuggestion {
                        line: *line,
                        col: *col,
                        reason: "no longer suggested: the file changed since it was analysed"
                            .to_string(),
                    }),
                }
            }
        }
    }
    if chosen.is_empty() {
        return (outcome, None);
    }

    let edits: Vec<Edit> = chosen
        .iter()
        .flat_map(|s| s.edits.iter().cloned())
        .collect();
    let skip_all = |outcome: &mut FileOutcome, reason: String| {
        for s in &chosen {
            outcome.skipped.push(SkippedSuggestion {
                line: s.line,
                col: s.col,
                reason: reason.clone(),
            });
        }
    };
    let Some(edited) = apply_edits(&analyzed.source, &edits) else {
        skip_all(
            &mut outcome,
            "its edits overlap or don't fit the file".to_string(),
        );
        return (outcome, None);
    };
    if parse_module(&edited).is_err() {
        skip_all(&mut outcome, "the edited file would not parse".to_string());
        return (outcome, None);
    }

    // Check the edited source exactly as the original was checked.
    let project_root = find_project_root(path);
    let config = load_linter_config(&project_root);
    let (mut linter, _) = linter_with_index(path, &project_root, &config, &edited, index);
    let after = match linter.check_file_internal(&edited, path) {
        Ok(errors) => error_keys(&errors),
        Err(e) => {
            skip_all(
                &mut outcome,
                format!("the edited file failed to check: {e}"),
            );
            return (outcome, None);
        }
    };
    outcome.errors_after = after.len();
    let introduced = new_errors(&analyzed.errors, &after);
    if introduced > 0 {
        skip_all(
            &mut outcome,
            format!(
                "re-checking the edited file found {introduced} new error{}",
                if introduced == 1 { "" } else { "s" }
            ),
        );
        return (outcome, None);
    }

    outcome.applied = chosen
        .iter()
        .map(|s| AppliedSuggestion {
            line: s.line,
            col: s.col,
            var: s.var.clone(),
            columns: s.columns.clone(),
        })
        .collect();
    (
        outcome,
        Some(Planned {
            path: path.to_path_buf(),
            original: analyzed.source,
            edited,
        }),
    )
}

fn temporary_sibling(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.typedframes-{}", std::process::id()))
}

// Written beside the target and renamed over it, so the file is never half-written.
fn write_atomically(path: &Path, content: &str) -> std::io::Result<()> {
    // Renaming over a symlink would replace the link, so write to what it points at.
    let path = &fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let temporary = temporary_sibling(path);
    fs::write(&temporary, content)
        .and_then(|()| {
            if let Ok(metadata) = fs::metadata(path) {
                let _ = fs::set_permissions(&temporary, metadata.permissions());
            }
            fs::rename(&temporary, path)
        })
        .inspect_err(|_| {
            let _ = fs::remove_file(&temporary);
        })
}

/// Writes every planned file, or none: a file changed on disk since it was analysed, or a
/// failed write, leaves everything as it was. Returns the reason on failure.
pub(crate) fn commit(planned: &[Planned]) -> Result<(), String> {
    for plan in planned {
        match fs::read_to_string(&plan.path) {
            Ok(current) if current == plan.original => {}
            _ => {
                return Err(format!(
                    "{} changed on disk since it was analysed; nothing was written",
                    plan.path.display()
                ))
            }
        }
    }
    let mut written: Vec<&Planned> = Vec::new();
    for plan in planned {
        if let Err(error) = write_atomically(&plan.path, &plan.edited) {
            for done in written.iter().rev() {
                let _ = write_atomically(&done.path, &done.original);
            }
            return Err(format!(
                "could not write {}: {error}; every file was restored",
                plan.path.display()
            ));
        }
        written.push(plan);
    }
    Ok(())
}

/// Why it isn't safe to edit files under `root`, or `None` when it is: editing is allowed
/// only in a git work tree whose tracked files have no uncommitted changes, so everything
/// done can be reviewed and undone with git.
pub(crate) fn vcs_problem(root: &Path) -> Option<String> {
    vcs_problem_within(root, None)
}

// `ceiling` stops git looking for a repository above that directory.
fn vcs_problem_within(root: &Path, ceiling: Option<&Path>) -> Option<String> {
    use std::process::Command;
    const NO_UNDO: &str =
        "so there would be no way to undo the edits (use --allow-dirty to go ahead anyway)";
    let git = |args: &[&str]| {
        let mut command = Command::new("git");
        command.arg("-C").arg(root).args(args);
        if let Some(ceiling) = ceiling {
            command.env("GIT_CEILING_DIRECTORIES", ceiling);
        }
        command.output()
    };
    match git(&["rev-parse", "--is-inside-work-tree"]) {
        Err(_) => return Some(format!("git is not available, {NO_UNDO}")),
        Ok(out)
            if !out.status.success() || String::from_utf8_lossy(&out.stdout).trim() != "true" =>
        {
            return Some(format!("not inside a git repository, {NO_UNDO}"));
        }
        Ok(_) => {}
    }
    let Some(status) = git(&["status", "--porcelain", "--untracked-files=no"])
        .ok()
        .filter(|out| out.status.success())
    else {
        return Some(format!("could not read the git status, {NO_UNDO}"));
    };
    let changed = String::from_utf8_lossy(&status.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    (changed > 0).then(|| {
        format!(
            "the git work tree has {changed} uncommitted change{}; commit or stash them first, or use --allow-dirty",
            if changed == 1 { "" } else { "s" }
        )
    })
}

#[derive(Serialize, Debug)]
pub struct ApplyResult {
    pub files: Vec<FileOutcome>,
    /// Set when the write step failed and every file was left as it was.
    pub failed: Option<String>,
}

fn is_tracked(path: &Path) -> bool {
    let Ok(real) = fs::canonicalize(path) else {
        return false;
    };
    let Some(dir) = real.parent() else {
        return false;
    };
    std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(&real)
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Plans every request in parallel, then commits all accepted files together.
pub(crate) fn apply_all(
    requests: &[(String, String, Selection)],
    index: Option<Arc<ProjectIndex>>,
    require_tracked: bool,
) -> ApplyResult {
    let planned = crate::parallel::parallel_map(requests, |(file, display, selection)| {
        plan_file(Path::new(file), display, selection, index.clone())
    });
    let mut files = Vec::with_capacity(planned.len());
    let mut plans: Vec<Planned> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (mut outcome, plan) in planned {
        let refusal = plan.as_ref().and_then(|p| {
            // Two requests for one file (say a symlink and its target) would overwrite each other.
            if !seen.insert(fs::canonicalize(&p.path).unwrap_or_else(|_| p.path.clone())) {
                Some("the same file is already part of this run".to_string())
            // An untracked file has no git history to restore it from.
            } else if require_tracked && !is_tracked(&p.path) {
                Some(format!(
                    "{} is not tracked by git, so the edit could not be undone (use --allow-dirty to go ahead anyway)",
                    p.path.display()
                ))
            } else {
                None
            }
        });
        if refusal.is_some() {
            outcome.applied.clear();
            outcome.error = refusal;
            files.push(outcome);
            continue;
        }
        files.push(outcome);
        plans.extend(plan);
    }
    let failed = commit(&plans).err();
    for outcome in &mut files {
        if failed.is_none() {
            outcome.written = !outcome.applied.is_empty();
        } else {
            for applied in outcome.applied.drain(..) {
                outcome.skipped.push(SkippedSuggestion {
                    line: applied.line,
                    col: applied.col,
                    reason: "not written: the write step failed and every file was restored"
                        .to_string(),
                });
            }
        }
    }
    ApplyResult { files, failed }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAFE: &str = "import pandas as pd\n\n\ndef run():\n    df = pd.read_csv(\"x.csv\")\n    return df[\"a\"] + df[\"b\"]\n";
    // read only on one path, so a review-tier suggestion
    const REVIEW: &str = "import pandas as pd\n\n\ndef run(flag):\n    df = pd.read_csv(\"x.csv\")\n    if flag:\n        return df[\"b\"]\n    return df[\"a\"]\n";

    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("pyproject.toml"), "").unwrap();
        for (name, source) in files {
            fs::write(dir.path().join(name), source).unwrap();
        }
        dir
    }

    fn request_in(dir: &Path, name: &str) -> (String, String, Selection) {
        (
            dir.join(name).to_string_lossy().into_owned(),
            name.to_string(),
            Selection::SafeTier,
        )
    }

    fn request(
        dir: &tempfile::TempDir,
        name: &str,
        selection: Selection,
    ) -> (String, String, Selection) {
        (
            dir.path().join(name).to_string_lossy().into_owned(),
            name.to_string(),
            selection,
        )
    }

    fn read(dir: &tempfile::TempDir, name: &str) -> String {
        fs::read_to_string(dir.path().join(name)).unwrap()
    }

    #[test]
    fn test_should_apply_edits_from_the_end_so_offsets_stay_valid() {
        let edits = vec![
            Edit {
                start: 1,
                end: 1,
                replacement: "X".to_string(),
            },
            Edit {
                start: 3,
                end: 3,
                replacement: "Y".to_string(),
            },
        ];

        assert_eq!(apply_edits("abcd", &edits).unwrap(), "aXbcYd");
    }

    #[test]
    fn test_should_refuse_overlapping_or_out_of_range_edits() {
        let overlapping = vec![
            Edit {
                start: 1,
                end: 3,
                replacement: String::new(),
            },
            Edit {
                start: 2,
                end: 2,
                replacement: "Z".to_string(),
            },
        ];
        let out_of_range = vec![Edit {
            start: 9,
            end: 9,
            replacement: "Z".to_string(),
        }];

        assert!(apply_edits("abcd", &overlapping).is_none());
        assert!(apply_edits("abcd", &out_of_range).is_none());
    }

    #[test]
    fn test_should_apply_the_safe_tier_and_write_the_file() {
        let dir = project(&[("a.py", SAFE)]);

        let result = apply_all(&[request(&dir, "a.py", Selection::SafeTier)], None, false);

        assert!(result.failed.is_none());
        assert!(result.files[0].written);
        assert_eq!(result.files[0].applied.len(), 1);
        assert_eq!(
            read(&dir, "a.py"),
            SAFE.replace("    return", "    df = df[[\"a\", \"b\"]]\n    return")
        );
    }

    #[test]
    fn test_should_not_apply_a_review_suggestion_unless_it_was_chosen() {
        let dir = project(&[("a.py", REVIEW)]);

        let result = apply_all(&[request(&dir, "a.py", Selection::SafeTier)], None, false);

        assert!(!result.files[0].written);
        assert_eq!(read(&dir, "a.py"), REVIEW);
    }

    #[test]
    fn test_should_apply_a_review_suggestion_that_was_chosen_by_position() {
        let dir = project(&[("a.py", REVIEW)]);
        let origin = (5, 5);

        let result = apply_all(
            &[request(&dir, "a.py", Selection::Origins(vec![origin]))],
            None,
            false,
        );

        assert!(result.files[0].written, "{:?}", result.files[0].skipped);
        assert!(read(&dir, "a.py").contains("    df = df[[\"b\", \"a\"]]\n"));
    }

    #[test]
    fn test_should_skip_a_chosen_suggestion_that_no_longer_exists() {
        let dir = project(&[("a.py", SAFE)]);

        let result = apply_all(
            &[request(&dir, "a.py", Selection::Origins(vec![(99, 1)]))],
            None,
            false,
        );

        assert!(!result.files[0].written);
        assert!(result.files[0].skipped[0].reason.contains("changed since"));
        assert_eq!(read(&dir, "a.py"), SAFE);
    }

    #[test]
    fn test_should_not_apply_the_same_suggestion_twice() {
        let dir = project(&[("a.py", SAFE)]);
        apply_all(&[request(&dir, "a.py", Selection::SafeTier)], None, false);
        let once = read(&dir, "a.py");

        let second = apply_all(&[request(&dir, "a.py", Selection::SafeTier)], None, false);

        assert!(!second.files[0].written, "{:?}", second.files[0]);
        assert_eq!(read(&dir, "a.py"), once);
    }

    #[test]
    fn test_should_apply_to_several_files_together() {
        let dir = project(&[("a.py", SAFE), ("b.py", SAFE)]);

        let result = apply_all(
            &[
                request(&dir, "a.py", Selection::SafeTier),
                request(&dir, "b.py", Selection::SafeTier),
            ],
            None,
            false,
        );

        assert!(result.files.iter().all(|f| f.written));
        assert!(read(&dir, "a.py").contains("df[[\"a\", \"b\"]]"));
        assert!(read(&dir, "b.py").contains("df[[\"a\", \"b\"]]"));
    }

    #[test]
    fn test_should_write_nothing_when_a_file_changed_since_it_was_analysed() {
        let dir = project(&[("a.py", SAFE)]);
        let path = dir.path().join("a.py");
        let plan = Planned {
            path: path.clone(),
            original: SAFE.to_string(),
            edited: format!("{SAFE}# edited\n"),
        };
        fs::write(&path, "# someone else's edit\n").unwrap();

        let result = commit(&[plan]);

        assert!(result.unwrap_err().contains("changed on disk"));
        assert_eq!(read(&dir, "a.py"), "# someone else's edit\n");
    }

    #[test]
    fn test_should_restore_every_file_when_a_later_write_fails() {
        let dir = project(&[("a.py", SAFE), ("b.py", SAFE)]);
        let plans = vec![
            Planned {
                path: dir.path().join("a.py"),
                original: SAFE.to_string(),
                edited: format!("{SAFE}# one\n"),
            },
            Planned {
                path: dir.path().join("b.py"),
                original: SAFE.to_string(),
                edited: format!("{SAFE}# two\n"),
            },
        ];
        // A directory where the second file's temporary sibling would go makes that write
        // fail whoever runs the test.
        fs::create_dir(temporary_sibling(&dir.path().join("b.py"))).unwrap();

        let result = commit(&plans);

        assert!(result.is_err());
        assert_eq!(read(&dir, "a.py"), SAFE);
        assert_eq!(read(&dir, "b.py"), SAFE);
    }

    #[test]
    fn test_should_leave_no_temporary_files_behind() {
        let dir = project(&[("a.py", SAFE)]);

        apply_all(&[request(&dir, "a.py", Selection::SafeTier)], None, false);

        let names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.iter().all(|n| !n.contains("typedframes-")),
            "{names:?}"
        );
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git runs");
        assert!(status.status.success(), "git {args:?}: {status:?}");
    }

    #[test]
    fn test_should_refuse_a_directory_that_is_not_a_git_work_tree() {
        let dir = tempfile::tempdir().unwrap();

        let ceiling = dir.path().parent().unwrap();

        let problem = vcs_problem_within(dir.path(), Some(ceiling)).unwrap();

        assert!(problem.contains("not inside a git repository"), "{problem}");
    }

    #[test]
    fn test_should_count_an_error_swapped_for_another_as_new() {
        let before = vec!["unknown-column\0no column 'a' (line )".to_string()];
        let same = before.clone();
        let swapped = vec!["unknown-column\0no column 'b' (line )".to_string()];

        assert_eq!(new_errors(&before, &same), 0);
        assert_eq!(new_errors(&before, &swapped), 1);
        assert_eq!(new_errors(&before, &[]), 0);
        assert_eq!(new_errors(&[], &same), 1);
    }

    #[test]
    fn test_should_ignore_line_numbers_when_comparing_errors() {
        let at = |line: usize| LintError {
            line,
            col: 1,
            code: "unknown-column".to_string(),
            message: format!("no column 'a' (defined at line {line})"),
            severity: "error".to_string(),
        };

        assert_eq!(error_keys(&[at(3)]), error_keys(&[at(14)]));
    }

    #[test]
    fn test_should_accept_a_clean_work_tree_and_refuse_a_dirty_one() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.py"), "x = 1\n").unwrap();
        git(dir.path(), &["init", "-q"]);
        git(dir.path(), &["add", "a.py"]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);
        assert_eq!(vcs_problem(dir.path()), None);

        fs::write(dir.path().join("a.py"), "x = 2\n").unwrap();
        let problem = vcs_problem(dir.path()).unwrap();

        assert!(problem.contains("1 uncommitted change"), "{problem}");
        // untracked files don't make the tree dirty; editing them is refused separately
        git(dir.path(), &["checkout", "-q", "--", "a.py"]);
        fs::write(dir.path().join("new.py"), "y = 1\n").unwrap();
        assert_eq!(vcs_problem(dir.path()), None);
    }

    #[test]
    fn test_should_only_strip_line_numbers_from_error_keys() {
        let error = |message: &str| LintError {
            line: 1,
            col: 1,
            code: "unknown-column".to_string(),
            message: message.to_string(),
            severity: "error".to_string(),
        };

        let q1 = error_keys(&[error("no column 'q1'")]);
        let q2 = error_keys(&[error("no column 'q2'")]);
        let moved_a = error_keys(&[error("defined at line 3 (a.py:3)")]);
        let moved_b = error_keys(&[error("defined at line 14 (a.py:14)")]);

        assert_ne!(q1, q2);
        assert_eq!(moved_a, moved_b);
    }

    #[test]
    fn test_should_refuse_to_edit_a_file_git_does_not_track() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("pyproject.toml"), "").unwrap();
        fs::write(dir.path().join("tracked.py"), SAFE).unwrap();
        git(dir.path(), &["init", "-q"]);
        git(dir.path(), &["add", "tracked.py"]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);
        fs::write(dir.path().join("loose.py"), SAFE).unwrap();
        let requests = [
            request_in(dir.path(), "tracked.py"),
            request_in(dir.path(), "loose.py"),
        ];

        let result = apply_all(&requests, None, true);

        assert!(result.files[0].written);
        assert!(!result.files[1].written);
        let error = result.files[1].error.as_deref().unwrap();
        assert!(error.contains("not tracked by git"), "{error}");
        assert_eq!(
            fs::read_to_string(dir.path().join("loose.py")).unwrap(),
            SAFE
        );
    }

    #[test]
    fn test_should_edit_an_untracked_file_when_tracking_is_not_required() {
        let dir = project(&[("a.py", SAFE)]);

        let result = apply_all(&[request(&dir, "a.py", Selection::SafeTier)], None, false);

        assert!(result.files[0].written);
    }

    #[cfg(unix)]
    #[test]
    fn test_should_write_through_a_symlink_and_keep_the_link() {
        let dir = project(&[("real.py", SAFE)]);
        std::os::unix::fs::symlink(dir.path().join("real.py"), dir.path().join("link.py")).unwrap();

        write_atomically(&dir.path().join("link.py"), "x = 1\n").unwrap();

        assert!(fs::symlink_metadata(dir.path().join("link.py"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(read(&dir, "real.py"), "x = 1\n");
    }

    #[test]
    fn test_should_write_a_file_requested_twice_only_once() {
        let dir = project(&[("a.py", SAFE)]);
        let requests = [
            request(&dir, "a.py", Selection::SafeTier),
            request(&dir, "a.py", Selection::SafeTier),
        ];

        let result = apply_all(&requests, None, false);

        assert!(result.files[0].written);
        assert!(!result.files[1].written);
        assert!(result.files[1].error.is_some());
        assert_eq!(read(&dir, "a.py").matches("df = df[[").count(), 1);
    }

    #[test]
    fn test_should_not_treat_other_text_before_a_number_as_a_line_reference() {
        assert_eq!(
            strip_line_numbers("see line 12 and a.py:34"),
            "see line  and a.py:"
        );
        assert_eq!(strip_line_numbers("pipeline 5 at x:7"), "pipeline 5 at x:7");
    }
}
