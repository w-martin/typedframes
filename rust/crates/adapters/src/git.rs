//! Editing a git working copy: atomic writes, and whether git can undo an edit.

use std::fs;
use std::path::{Path, PathBuf};
use typedframes_domain::ports::Workspace;

/// A working copy under git.
#[derive(Debug, Default, Clone, Copy)]
pub struct GitWorkspace;

fn temporary_sibling(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.typedframes-{}", std::process::id()))
}

impl Workspace for GitWorkspace {
    fn is_tracked(&self, path: &Path) -> bool {
        let Ok(real) = std::fs::canonicalize(path) else {
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

    // Written beside the target and renamed over it, so the file is never half-written.
    fn write_atomically(&self, path: &Path, content: &str) -> std::io::Result<()> {
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
}

/// Why it isn't safe to edit files under `root`, or `None` when it is: editing is allowed
/// only in a git work tree whose tracked files have no uncommitted changes, so everything
/// done can be reviewed and undone with git.
pub fn vcs_problem(root: &Path) -> Option<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[cfg(unix)]
    #[test]
    fn test_should_write_through_a_symlink_and_keep_the_link() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("real.py"), "y = 2\n").unwrap();
        std::os::unix::fs::symlink(dir.path().join("real.py"), dir.path().join("link.py")).unwrap();

        GitWorkspace
            .write_atomically(&dir.path().join("link.py"), "x = 1\n")
            .unwrap();

        assert!(fs::symlink_metadata(dir.path().join("link.py"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read_to_string(dir.path().join("real.py")).unwrap(),
            "x = 1\n"
        );
    }
}
