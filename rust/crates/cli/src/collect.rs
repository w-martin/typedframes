//! Finding the files to check.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

// Directories that are never descended into by default: VCS metadata, virtualenvs, caches,
// editor and tool state, and vendored or build trees. This mirrors ruff's default exclude list,
// plus `target`, the Rust/maturin build output. `[tool.typedframes] exclude` REPLACES this set
// entirely rather than adding to it, as ruff's own `exclude` does.
const EXCLUDED_DIRS: &[&str] = &[
    ".bzr",
    ".claude",
    ".direnv",
    ".eggs",
    ".git",
    ".git-rewrite",
    ".hg",
    ".ipynb_checkpoints",
    ".mypy_cache",
    ".nox",
    ".pants.d",
    ".pytest_cache",
    ".pytype",
    ".ruff_cache",
    ".svn",
    ".tox",
    ".venv",
    ".vscode",
    ".idea",
    "__pycache__",
    "_build",
    "buck-out",
    "build",
    "dist",
    "node_modules",
    "site-packages",
    "target",
    "venv",
];

// Python's `Path` ordering: component by component, so `a.py` sorts after `a/b.py` (as text it
// would not). On Windows paths compare case-insensitively.
fn path_order(a: &PathBuf, b: &PathBuf) -> std::cmp::Ordering {
    if cfg!(windows) {
        let lower = |p: &PathBuf| PathBuf::from(p.to_string_lossy().to_lowercase());
        lower(a).components().cmp(lower(b).components())
    } else {
        a.components().cmp(b.components())
    }
}

fn has_suffix(name: &str, suffixes: &[&str]) -> bool {
    suffixes.iter().any(|suffix| name.ends_with(suffix))
}

/// Every file ending in one of `suffixes` under `path` (a file or a directory), in one walk and in
/// path order. `configured_excludes` replaces the built-in set of skipped directories when given.
pub(crate) fn collect_files(
    path: &Path,
    suffixes: &[&str],
    configured_excludes: Option<&HashSet<String>>,
) -> Vec<PathBuf> {
    if path.is_file() {
        let matches = path
            .extension()
            .map(|ext| format!(".{}", ext.to_string_lossy()))
            .is_some_and(|ext| suffixes.contains(&ext.as_str()));
        return if matches {
            vec![path.to_path_buf()]
        } else {
            Vec::new()
        };
    }

    let skip = |name: &str| match configured_excludes {
        Some(set) => set.contains(name),
        None => EXCLUDED_DIRS.contains(&name),
    };
    let mut found = Vec::new();
    let mut pending = vec![path.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let entry_path = entry.path();
            // A symlink to a directory is listed with the directories and not descended into.
            let is_dir = fs::metadata(&entry_path).is_ok_and(|m| m.is_dir());
            if is_dir {
                let is_symlink = entry.file_type().is_ok_and(|t| t.is_symlink());
                if !is_symlink && !skip(&name) {
                    pending.push(entry_path);
                }
            } else if has_suffix(&name, suffixes) {
                found.push(entry_path);
            }
        }
    }
    found.sort_by(path_order);
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(root: &Path, relative: &str) {
        let file = root.join(relative);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, "x = 1\n").unwrap();
    }

    fn names(root: &Path, files: &[PathBuf]) -> Vec<String> {
        files
            .iter()
            .map(|f| {
                f.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect()
    }

    #[test]
    fn test_should_sort_a_file_after_the_directory_that_shares_its_stem() {
        // arrange
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "a.py");
        touch(dir.path(), "a/b.py");
        touch(dir.path(), "a-b/c.py");

        // act
        let files = collect_files(dir.path(), &[".py"], None);

        // assert: by component `a` < `a-b` < `a.py`, as `Path` objects compared in Python.
        assert_eq!(
            vec!["a/b.py", "a-b/c.py", "a.py"],
            names(dir.path(), &files)
        );
    }

    #[test]
    fn test_should_skip_the_default_directories() {
        // arrange
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "src/a.py");
        touch(dir.path(), ".venv/lib/b.py");
        touch(dir.path(), "node_modules/c.py");

        // act
        let files = collect_files(dir.path(), &[".py"], None);

        // assert
        assert_eq!(vec!["src/a.py"], names(dir.path(), &files));
    }

    #[test]
    fn test_should_replace_the_default_set_with_the_configured_one() {
        // arrange
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "legacy/a.py");
        touch(dir.path(), "build/b.py");
        let excludes = HashSet::from(["legacy".to_string()]);

        // act
        let files = collect_files(dir.path(), &[".py"], Some(&excludes));

        // assert
        assert_eq!(vec!["build/b.py"], names(dir.path(), &files));
    }

    #[test]
    fn test_should_collect_python_files_and_notebooks_together() {
        // arrange
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "a.py");
        touch(dir.path(), "n.ipynb");
        touch(dir.path(), "notes.txt");

        // act
        let files = collect_files(dir.path(), &[".py", ".ipynb"], None);

        // assert
        assert_eq!(vec!["a.py", "n.ipynb"], names(dir.path(), &files));
    }

    #[test]
    fn test_should_return_a_single_file_only_when_it_has_a_wanted_suffix() {
        // arrange
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "a.py");
        touch(dir.path(), "b.txt");

        // act
        let wanted = collect_files(&dir.path().join("a.py"), &[".py"], None);
        let unwanted = collect_files(&dir.path().join("b.txt"), &[".py"], None);

        // assert
        assert_eq!(1, wanted.len());
        assert!(unwanted.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn test_should_not_descend_into_a_symlinked_directory() {
        // arrange
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "real/a.py");
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("link")).unwrap();

        // act
        let files = collect_files(dir.path(), &[".py"], None);

        // assert
        assert_eq!(vec!["real/a.py"], names(dir.path(), &files));
    }
}
