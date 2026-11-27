//! Plans every file's edits, re-checks the edited source, then writes all accepted files
//! or none. Edits are byte ranges against the analysed source, so each file is re-read
//! before writing and left alone if it changed.

use crate::engine::{is_notebook, load_python_notebook, CheckError};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use typedframes_domain::analysis::FileContext;
use typedframes_domain::config::{import_root, load_linter_config};
pub use typedframes_domain::edits::Selection;
use typedframes_domain::edits::{plan, FileOutcome, SkippedSuggestion};
use typedframes_domain::index::ProjectIndex;
use typedframes_domain::ports::FileSystem;
use typedframes_domain::ports::Workspace;

/// An accepted change to one file, ready to write.
pub(crate) struct Planned {
    path: PathBuf,
    original: String,
    edited: String,
}

/// Works out what to do for one file. Nothing is written here.
pub(crate) fn plan_file(
    fs: &Arc<dyn FileSystem>,
    path: &Path,
    display_path: &str,
    selection: &Selection,
    index: Option<Arc<ProjectIndex>>,
) -> (FileOutcome, Option<Planned>) {
    let project_root = import_root(&**fs, path);
    let config = load_linter_config(&**fs, &project_root);
    let outcome = FileOutcome {
        file: display_path.to_string(),
        ..FileOutcome::default()
    };
    if !config.enabled.unwrap_or(true) {
        return (outcome, None);
    }
    let loaded = fs
        .read_to_string(path)
        .map_err(|e| CheckError::Io(e.to_string()))
        .and_then(|raw| {
            let notebook = if is_notebook(path) {
                Some(load_python_notebook(&**fs, path)?)
            } else {
                None
            };
            Ok((raw, notebook))
        });
    let (raw, notebook) = match loaded {
        Ok(loaded) => loaded,
        Err(error) => {
            return (
                FileOutcome {
                    error: Some(error.message().to_string()),
                    ..outcome
                },
                None,
            )
        }
    };
    let context = FileContext {
        fs: Arc::clone(fs),
        path,
        project_root: &project_root,
        config: &config,
        index,
    };
    let (outcome, edited) = plan(&context, display_path, selection, raw.clone(), notebook);
    let planned = edited.map(|edited| Planned {
        path: path.to_path_buf(),
        original: raw,
        edited,
    });
    (outcome, planned)
}

/// Writes every planned file, or none: a file changed on disk since it was analysed, or a
/// failed write, leaves everything as it was. Returns the reason on failure.
pub(crate) fn commit(
    fs: &dyn FileSystem,
    workspace: &dyn Workspace,
    planned: &[Planned],
) -> Result<(), String> {
    for plan in planned {
        match fs.read_to_string(&plan.path) {
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
        if let Err(error) = workspace.write_atomically(&plan.path, &plan.edited) {
            for done in written.iter().rev() {
                let _ = workspace.write_atomically(&done.path, &done.original);
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

#[derive(Serialize, Debug)]
pub struct ApplyResult {
    pub files: Vec<FileOutcome>,
    /// Set when the write step failed and every file was left as it was.
    pub failed: Option<String>,
}

/// Plans every request in parallel, then commits all accepted files together.
pub fn apply_all(
    fs: &Arc<dyn FileSystem>,
    workspace: &dyn Workspace,
    requests: &[(String, String, Selection)],
    index: Option<Arc<ProjectIndex>>,
    require_tracked: bool,
) -> ApplyResult {
    let planned =
        typedframes_domain::parallel::parallel_map(requests, |(file, display, selection)| {
            plan_file(fs, Path::new(file), display, selection, index.clone())
        });
    let mut files = Vec::with_capacity(planned.len());
    let mut plans: Vec<Planned> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (mut outcome, plan) in planned {
        let refusal = plan.as_ref().and_then(|p| {
            // Two requests for one file (say a symlink and its target) would overwrite each other.
            if !seen.insert(fs.canonicalize(&p.path).unwrap_or_else(|_| p.path.clone())) {
                Some("the same file is already part of this run".to_string())
            // An untracked file has no git history to restore it from.
            } else if require_tracked && !workspace.is_tracked(&p.path) {
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
    let failed = commit(&**fs, workspace, &plans).err();
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
                    cell: applied.cell,
                });
            }
        }
    }
    ApplyResult { files, failed }
}

#[cfg(test)]
mod tests;
