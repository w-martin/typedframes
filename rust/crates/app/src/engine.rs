//! The Python-free core the extension and the command line both call: per-file checks,
//! suggestions and reports, the project contexts they share, and the process-wide
//! deserialised-index cache.

use ruff_notebook::{Notebook, NotebookError};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use typedframes_domain::analysis::{self, FileContext, Suggestions};
use typedframes_domain::config::{
    find_project_root_opt, import_root, load_linter_config, LinterConfig,
};
use typedframes_domain::errors::{CheckFileResult, FileStats};
use typedframes_domain::index::ProjectIndex;
use typedframes_domain::notebook::{NotebookCheckResult, NotebookFileStats};
use typedframes_domain::parallel::parallel_map_largest_first;
use typedframes_domain::ports::FileSystem;

pub(crate) fn is_notebook(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "ipynb")
}

/// The notebook at `path`, which must be a Python one.
pub(crate) fn load_python_notebook(
    fs: &dyn FileSystem,
    path: &Path,
) -> Result<Notebook, CheckError> {
    let bytes = fs.read(path).map_err(|e| CheckError::Io(e.to_string()))?;
    let notebook = typedframes_domain::notebook::parse(&bytes).map_err(|e| match e {
        NotebookError::Io(io_err) => CheckError::Io(io_err.to_string()),
        other => runtime(other),
    })?;
    if !notebook.is_python_notebook() {
        return Err(runtime("not a Python notebook (kernel is not Python)"));
    }
    Ok(notebook)
}

fn read_source(fs: &dyn FileSystem, path: &Path) -> Result<String, CheckError> {
    fs.read_to_string(path)
        .map_err(|e| CheckError::Io(e.to_string()))
}

/// A file's size, as the cost of working on it; unreadable files count as free.
pub(crate) fn file_cost(fs: &dyn FileSystem, path: &Path) -> u64 {
    fs.metadata(path).map_or(0, |meta| meta.len)
}

pub(crate) fn suggest_file_impl(
    file_path: &str,
    display_path: &str,
    index: Option<Arc<ProjectIndex>>,
    contexts: &ProjectContexts,
) -> Result<String, CheckError> {
    let path = Path::new(file_path);
    let project = contexts.of(path);
    let context = project.file(&contexts.fs, path, index);
    let result = if !project.config.enabled.unwrap_or(true) {
        Suggestions::default()
    } else if is_notebook(path) {
        let notebook = load_python_notebook(&*contexts.fs, path)?;
        analysis::suggest_notebook(&context, display_path, &notebook).map_err(runtime)?
    } else {
        let source = read_source(&*contexts.fs, path)?;
        analysis::suggest_python(&context, display_path, &source).map_err(runtime)?
    };
    serde_json::to_string(&result).map_err(runtime)
}

// Runs `f` over `items` in parallel, largest file first, as `(json, error)` pairs in input
// order. `path_of` names the file an item is about, for ordering by size.
pub(crate) fn run_batch<T: Sync>(
    fs: &dyn FileSystem,
    items: &[T],
    path_of: impl Fn(&T) -> &String,
    f: impl Fn(&T) -> Result<String, CheckError> + Sync,
) -> Vec<(Option<String>, Option<String>)> {
    parallel_map_largest_first(
        items,
        |item| file_cost(fs, Path::new(path_of(item))),
        |item| match f(item) {
            Ok(json) => (Some(json), None),
            Err(error) => (None, Some(error.message().to_string())),
        },
    )
}

// The file's source, read as `report` and `report_all` need it. `None` when the linter is
// disabled in the project's `pyproject.toml`.
fn source_for_report(
    fs: &dyn FileSystem,
    path: &Path,
    project: &ProjectContext,
) -> Result<Option<String>, CheckError> {
    if !project.config.enabled.unwrap_or(true) {
        return Ok(None);
    }
    read_source(fs, path).map(Some)
}

pub(crate) fn report_file_impl(
    file_path: &str,
    name: &str,
    index: Option<Arc<ProjectIndex>>,
    contexts: &ProjectContexts,
) -> Result<String, CheckError> {
    let path = Path::new(file_path);
    let project = contexts.of(path);
    let report = match source_for_report(&*contexts.fs, path, &project)? {
        Some(source) => {
            analysis::report_site(&project.file(&contexts.fs, path, index), &source, name)
                .map_err(runtime)?
        }
        None => None,
    };
    serde_json::to_string(&report).map_err(runtime)
}

pub(crate) fn report_all_file_impl(
    file_path: &str,
    index: Option<Arc<ProjectIndex>>,
    contexts: &ProjectContexts,
    untracked: bool,
) -> Result<String, CheckError> {
    let path = Path::new(file_path);
    let project = contexts.of(path);
    let reports = match source_for_report(&*contexts.fs, path, &project)? {
        Some(source) => {
            analysis::report_sites(&project.file(&contexts.fs, path, index), &source, untracked)
                .map_err(runtime)?
        }
        None => Vec::new(),
    };
    serde_json::to_string(&reports).map_err(runtime)
}

/// Why checking one file produced no result. Kept free of Python types so it can be
/// produced on a worker thread; [`check_file`]/[`check_notebook`] map it to `OSError` /
/// `RuntimeError`.
pub(crate) enum CheckError {
    /// The file couldn't be read.
    Io(String),
    /// The file isn't something the checker can parse (syntax error, bad notebook).
    Runtime(String),
}

impl CheckError {
    pub(crate) fn message(&self) -> &str {
        match self {
            CheckError::Io(m) | CheckError::Runtime(m) => m,
        }
    }
}

pub(crate) fn runtime(e: impl std::fmt::Display) -> CheckError {
    CheckError::Runtime(e.to_string())
}

/// A file's check result as JSON, or the message of why it was skipped: exactly one is set.
pub type FileOutcome = (Option<String>, Option<String>);

/// Checks `files` in parallel, largest first, as one outcome per file in input order. A path
/// ending in `.ipynb` is checked as a notebook.
pub fn check_batch(
    fs: &Arc<dyn FileSystem>,
    files: &[String],
    index: Option<Arc<ProjectIndex>>,
) -> Vec<FileOutcome> {
    let contexts = ProjectContexts::new(Arc::clone(fs));
    parallel_map_largest_first(
        files,
        |file_path| file_cost(&**fs, Path::new(file_path)),
        |file_path| {
            let result = if file_path.ends_with(".ipynb") {
                check_notebook_impl(&contexts.fs, file_path, index.clone())
            } else {
                check_file_impl(file_path, index.clone(), &contexts)
            };
            match result {
                Ok(json) => (Some(json), None),
                Err(error) => (None, Some(error.message().to_string())),
            }
        },
    )
}

/// Dry-run suggestions for each `(file, display name)` request, in parallel, as one outcome per
/// request in input order. The display name only names the file in diff headers.
pub fn suggest_batch(
    fs: &Arc<dyn FileSystem>,
    requests: &[(String, String)],
    index: Option<Arc<ProjectIndex>>,
) -> Vec<FileOutcome> {
    let contexts = ProjectContexts::new(Arc::clone(fs));
    run_batch(
        &**fs,
        requests,
        |(file, _)| file,
        |(file, display)| suggest_file_impl(file, display, index.clone(), &contexts),
    )
}

/// The report for the untyped site called `name` in each file (JSON `null` where a file has none),
/// in parallel, as one outcome per file in input order.
pub fn report_batch(
    fs: &Arc<dyn FileSystem>,
    files: &[String],
    name: &str,
    index: Option<Arc<ProjectIndex>>,
) -> Vec<FileOutcome> {
    let contexts = ProjectContexts::new(Arc::clone(fs));
    run_batch(
        &**fs,
        files,
        |file| file,
        |file| report_file_impl(file, name, index.clone(), &contexts),
    )
}

/// Every untyped site's report in each file (a JSON array), or with `untracked` the report of every
/// frame the checker does not track, in parallel, as one outcome per file in input order.
pub fn report_all_batch(
    fs: &Arc<dyn FileSystem>,
    files: &[String],
    index: Option<Arc<ProjectIndex>>,
    untracked: bool,
) -> Vec<FileOutcome> {
    let contexts = ProjectContexts::new(Arc::clone(fs));
    run_batch(
        &**fs,
        files,
        |file| file,
        |file| report_all_file_impl(file, index.clone(), &contexts, untracked),
    )
}

// A project's root and `[tool.typedframes]` settings.
pub(crate) struct ProjectContext {
    root: PathBuf,
    config: LinterConfig,
}

// The root and settings of the project each file belongs to, found once per directory
// rather than once per file: a batch of files shares a handful of directories, and reading
// and parsing `pyproject.toml` for every one of them is pure repetition. A directory with no
// `pyproject.toml` above it has no configuration; its files resolve imports from their
// `package_root`, and only the fact that no root exists is remembered.
pub(crate) struct ProjectContexts {
    fs: Arc<dyn FileSystem>,
    by_dir: RwLock<HashMap<PathBuf, Option<Arc<ProjectContext>>>>,
    by_root: RwLock<HashMap<PathBuf, Arc<ProjectContext>>>,
    roots: typedframes_domain::config::PackageRoots,
}

impl ProjectContexts {
    pub(crate) fn new(fs: Arc<dyn FileSystem>) -> Self {
        ProjectContexts {
            fs,
            by_dir: RwLock::default(),
            by_root: RwLock::default(),
            roots: typedframes_domain::config::PackageRoots::default(),
        }
    }

    fn of(&self, path: &Path) -> Arc<ProjectContext> {
        let dir = path.parent().unwrap_or(path);
        let cached = self.by_dir.read().ok().and_then(|m| m.get(dir).cloned());
        let found = cached.unwrap_or_else(|| {
            let found = find_project_root_opt(&*self.fs, path).map(|root| self.shared(root));
            if let Ok(mut dirs) = self.by_dir.write() {
                dirs.insert(dir.to_path_buf(), found.clone());
            }
            found
        });
        found.unwrap_or_else(|| {
            Arc::new(ProjectContext {
                root: self.roots.of_file(&*self.fs, path),
                config: LinterConfig::EMPTY,
            })
        })
    }

    fn shared(&self, root: PathBuf) -> Arc<ProjectContext> {
        if let Some(known) = self.by_root.read().ok().and_then(|m| m.get(&root).cloned()) {
            return known;
        }
        let context = Arc::new(ProjectContext {
            config: load_linter_config(&*self.fs, &root),
            root: root.clone(),
        });
        if let Ok(mut roots) = self.by_root.write() {
            roots.insert(root, Arc::clone(&context));
        }
        context
    }
}

impl ProjectContext {
    /// `path` in this project, with `index`.
    pub(crate) fn file<'a>(
        &'a self,
        fs: &Arc<dyn FileSystem>,
        path: &'a Path,
        index: Option<Arc<ProjectIndex>>,
    ) -> FileContext<'a> {
        FileContext {
            fs: Arc::clone(fs),
            path,
            project_root: &self.root,
            config: &self.config,
            index,
        }
    }
}

pub(crate) fn check_file_impl(
    file_path: &str,
    index: Option<Arc<ProjectIndex>>,
    contexts: &ProjectContexts,
) -> Result<String, CheckError> {
    let path = Path::new(file_path);
    let project = contexts.of(path);
    if !project.config.enabled.unwrap_or(true) {
        let empty = CheckFileResult {
            errors: Vec::new(),
            stats: FileStats::default(),
        };
        return serde_json::to_string(&empty).map_err(runtime);
    }
    let source = read_source(&*contexts.fs, path)?;
    let result = analysis::check_python(&project.file(&contexts.fs, path, index), &source)
        .map_err(runtime)?;
    serde_json::to_string(&result).map_err(runtime)
}

pub(crate) fn check_notebook_impl(
    fs: &Arc<dyn FileSystem>,
    file_path: &str,
    index: Option<Arc<ProjectIndex>>,
) -> Result<String, CheckError> {
    let path = Path::new(file_path);
    let root = import_root(&**fs, path);
    let project = ProjectContext {
        config: load_linter_config(&**fs, &root),
        root,
    };
    if !project.config.enabled.unwrap_or(true) {
        let empty = NotebookCheckResult {
            errors: Vec::new(),
            stats: NotebookFileStats::default(),
        };
        return serde_json::to_string(&empty).map_err(runtime);
    }
    let notebook = load_python_notebook(&**fs, path)?;
    let result =
        analysis::check_notebook(&project.file(fs, path, index), &notebook).map_err(runtime)?;
    serde_json::to_string(&result).map_err(runtime)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn files() -> Arc<dyn FileSystem> {
        Arc::new(typedframes_adapters::OsFiles)
    }

    #[test]
    fn test_should_root_a_file_with_no_project_file_above_it_where_python_imports_it_from() {
        // arrange: a flat script and a module inside a package, no `pyproject.toml`
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("a.py");
        let module = dir.path().join("pkg/sub/b.py");
        fs::create_dir_all(module.parent().unwrap()).unwrap();
        fs::write(&script, "x = 1\n").unwrap();
        fs::write(&module, "y = 2\n").unwrap();
        fs::write(dir.path().join("pkg/__init__.py"), "").unwrap();
        fs::write(dir.path().join("pkg/sub/__init__.py"), "").unwrap();
        let contexts = ProjectContexts::new(files());

        // act
        let (first, second) = (contexts.of(&script), contexts.of(&module));

        // assert: both from the directory above the package
        assert_eq!(first.root, dir.path());
        assert_eq!(second.root, dir.path());
    }

    #[test]
    fn test_should_share_one_context_between_files_of_the_same_project() {
        // arrange
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("pyproject.toml"), "").unwrap();
        let (a, b) = (dir.path().join("a.py"), dir.path().join("b.py"));
        fs::write(&a, "x = 1\n").unwrap();
        fs::write(&b, "y = 2\n").unwrap();
        let contexts = ProjectContexts::new(files());

        // act
        let (first, second) = (contexts.of(&a), contexts.of(&b));

        // assert
        assert_eq!(first.root, dir.path());
        assert!(Arc::ptr_eq(&first, &second));
    }
}
