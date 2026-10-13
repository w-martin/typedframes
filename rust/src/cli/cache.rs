//! Where the on-disk index cache lives, and the `cache` command's housekeeping around it.
//!
//! The checker reuses a previous run's per-file index for every file whose source is unchanged.
//! This module only decides where that cache is kept and offers the commands for finding,
//! clearing and gitignoring it; the cache's contents are the index's business.

use super::pathutil::{expanduser, posix, resolve};
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const ENV_VAR: &str = "TYPEDFRAMES_CACHE_DIR";

/// The per-user cache directory for this platform.
pub(crate) fn default_cache_dir() -> PathBuf {
    let home = || expanduser(Path::new("~"));
    let var = |name: &str| {
        env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if cfg!(target_os = "macos") {
        home().join("Library").join("Caches").join("typedframes")
    } else if cfg!(windows) {
        var("LOCALAPPDATA")
            .unwrap_or_else(|| home().join("AppData").join("Local"))
            .join("typedframes")
            .join("Cache")
    } else {
        var("XDG_CACHE_HOME")
            .unwrap_or_else(|| home().join(".cache"))
            .join("typedframes")
    }
}

/// The files in `cache_dir` that this tool wrote: never anything else in it.
pub(crate) fn cache_files(cache_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(cache_dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let is_cache_name = name
                .strip_prefix("index-")
                .is_some_and(|rest| rest.ends_with(".bin") || rest.contains(".tmp"));
            is_cache_name && entry.path().is_file()
        })
        .map(|entry| entry.path())
        .collect();
    files.sort();
    files
}

/// Makes the cache files this tool wrote readable by the user alone, and the default directory
/// too. The cache holds source paths and identifiers. A directory chosen with `--cache-dir`, the
/// environment or pyproject.toml may be shared (it can even be the project root), so there only
/// the tool's own files are touched; the per-user default directory is the tool's alone.
pub(crate) fn tighten_cache_permissions(cache_dir: &Path) {
    #[cfg(unix)]
    {
        for file in cache_files(cache_dir) {
            restrict(&file, 0o700);
        }
        if cache_dir.is_dir() && resolve(cache_dir) == resolve(&default_cache_dir()) {
            restrict(cache_dir, 0o700);
        }
    }
    #[cfg(not(unix))]
    let _ = cache_dir;
}

// Drops every permission bit outside `keep`, if the file isn't already that private.
#[cfg(unix)]
fn restrict(path: &Path, keep: u32) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    let mode = metadata.permissions().mode() & 0o777;
    if mode & !keep != 0 {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode & keep));
    }
}

/// The nearest directory at or above `path` that holds a `pyproject.toml`.
pub(crate) fn project_root_for(path: &Path) -> Option<PathBuf> {
    let start = resolve(path);
    start
        .ancestors()
        .find(|candidate| candidate.is_dir() && candidate.join("pyproject.toml").is_file())
        .map(Path::to_path_buf)
}

// `cache_dir` from `[tool.typedframes]` in the project's pyproject.toml, if set; a relative value
// is relative to the project root.
fn configured_cache_dir(root: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(root.join("pyproject.toml")).ok()?;
    let data: toml::Table = text.parse().ok()?;
    let value = data
        .get("tool")?
        .get("typedframes")?
        .get("cache_dir")?
        .as_str()
        .filter(|v| !v.is_empty())?;
    let configured = expanduser(Path::new(value));
    Some(if configured.is_absolute() {
        configured
    } else {
        root.join(configured)
    })
}

/// The cache directory for a run over `path`: `--cache-dir`, the `TYPEDFRAMES_CACHE_DIR`
/// environment variable, `cache_dir` in `[tool.typedframes]`, then the per-user default.
pub(crate) fn locate_cache_dir(path: &Path, cli_value: Option<&Path>) -> PathBuf {
    if let Some(value) = cli_value {
        return resolve(&expanduser(value));
    }
    if let Some(from_env) = env::var_os(ENV_VAR).filter(|v| !v.is_empty()) {
        return resolve(&expanduser(Path::new(&from_env)));
    }
    if let Some(configured) = project_root_for(path).and_then(|root| configured_cache_dir(&root)) {
        return resolve(&configured);
    }
    default_cache_dir()
}

/// `locate_cache_dir`, or `None` when `--no-cache` turned caching off.
pub(crate) fn resolve_cache_dir(
    path: &Path,
    cli_value: Option<&Path>,
    disabled: bool,
) -> Option<PathBuf> {
    (!disabled).then(|| locate_cache_dir(path, cli_value))
}

// The `.gitignore` line for `cache_dir`, or `None` when it is outside the project (or is the
// project, which would ignore all of it).
fn gitignore_entry(cache_dir: &Path, root: &Path) -> Option<String> {
    let relative = resolve(cache_dir)
        .strip_prefix(resolve(root))
        .ok()?
        .to_path_buf();
    if relative.as_os_str().is_empty() {
        return None;
    }
    Some(format!("/{}/", posix(&relative)))
}

// Appends `entry` to `root/.gitignore` unless it is already there; true if it was added. Works on
// bytes so a file that is not valid UTF-8 is written back as it was.
fn add_to_gitignore(root: &Path, entry: &str) -> std::io::Result<bool> {
    let gitignore = root.join(".gitignore");
    let existing = if gitignore.is_file() {
        fs::read(&gitignore)?
    } else {
        Vec::new()
    };
    let bare = entry.trim_matches('/');
    let present = existing.split(|&byte| byte == b'\n').any(|line| {
        let line = line.trim_ascii();
        line == entry.as_bytes() || line == bare.as_bytes()
    });
    if present {
        return Ok(false);
    }
    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with(b"\n") {
        updated.push(b'\n');
    }
    updated.extend_from_slice(entry.as_bytes());
    updated.push(b'\n');
    fs::write(&gitignore, updated)?;
    Ok(true)
}

/// What `typedframes cache ACTION` does.
#[derive(Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum)]
pub(crate) enum CacheAction {
    Path,
    Clear,
    Gitignore,
}

fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        format!("{count} {word}")
    } else {
        format!("{count} {word}s")
    }
}

/// Runs `typedframes cache`, writing its report to `out`.
pub(crate) fn run_cache(
    action: CacheAction,
    path: &Path,
    cache_dir_arg: Option<&Path>,
    out: &mut impl Write,
) -> std::io::Result<()> {
    let cache_dir = locate_cache_dir(path, cache_dir_arg);
    match action {
        CacheAction::Path => writeln!(out, "{}", cache_dir.display()),
        CacheAction::Clear => {
            let files = cache_files(&cache_dir);
            let mut freed: u64 = 0;
            for file in &files {
                freed += fs::metadata(file)?.len();
                fs::remove_file(file)?;
            }
            writeln!(
                out,
                "Removed {} ({:.0} KiB) from {}",
                plural(files.len(), "cache file"),
                freed as f64 / 1024.0,
                cache_dir.display()
            )
        }
        CacheAction::Gitignore => {
            let root = project_root_for(path);
            if let Some(root) = &root {
                if resolve(&cache_dir) == resolve(root) {
                    return writeln!(
                        out,
                        "The cache directory is the project root ({}); not gitignoring the whole project.",
                        root.display()
                    );
                }
            }
            let entry = root
                .as_ref()
                .and_then(|root| gitignore_entry(&cache_dir, root));
            let (Some(root), Some(entry)) = (root, entry) else {
                return writeln!(
                    out,
                    "The cache directory ({}) is outside the project, so there is nothing to gitignore.\n\
                     To keep it inside the project instead, use --cache-dir .typedframes_cache or set\n\
                     `cache_dir = \".typedframes_cache\"` under [tool.typedframes] in pyproject.toml,\n\
                     then run `typedframes cache gitignore` again.",
                    cache_dir.display()
                );
            };
            let added = add_to_gitignore(&root, &entry)?;
            writeln!(
                out,
                "{} {}: {}",
                if added { "Added" } else { "Already in" },
                root.join(".gitignore").display(),
                entry
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(action: CacheAction, path: &Path, cache_dir: Option<&Path>) -> String {
        let mut out = Vec::new();
        run_cache(action, path, cache_dir, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn test_should_prefer_the_command_line_directory_over_the_project_setting() {
        // arrange
        let project = tempfile::tempdir().unwrap();
        fs::write(
            project.path().join("pyproject.toml"),
            "[tool.typedframes]\ncache_dir = \"configured\"\n",
        )
        .unwrap();
        let chosen = tempfile::tempdir().unwrap();

        // act
        let located = locate_cache_dir(project.path(), Some(chosen.path()));

        // assert
        assert_eq!(resolve(chosen.path()), located);
    }

    #[test]
    fn test_should_read_a_relative_cache_dir_from_the_project_file() {
        // arrange
        let project = tempfile::tempdir().unwrap();
        fs::write(
            project.path().join("pyproject.toml"),
            "[tool.typedframes]\ncache_dir = \".tf_cache\"\n",
        )
        .unwrap();

        // act
        let configured = configured_cache_dir(project.path());

        // assert
        assert_eq!(Some(project.path().join(".tf_cache")), configured);
    }

    #[test]
    fn test_should_remove_only_the_files_the_tool_wrote() {
        // arrange
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("index-ab.bin"), vec![0u8; 2048]).unwrap();
        fs::write(dir.path().join("index-ab.tmp123"), b"x").unwrap();
        fs::write(dir.path().join("notes.txt"), b"keep").unwrap();

        // act
        let report = run(CacheAction::Clear, dir.path(), Some(dir.path()));

        // assert
        assert!(report.starts_with("Removed 2 cache files (2 KiB) from "));
        assert!(dir.path().join("notes.txt").exists());
        assert!(cache_files(dir.path()).is_empty());
    }

    #[test]
    fn test_should_add_a_project_local_cache_to_gitignore_once() {
        // arrange
        let project = tempfile::tempdir().unwrap();
        fs::write(project.path().join("pyproject.toml"), "").unwrap();
        let cache = project.path().join(".tf_cache");

        // act
        let first = run(CacheAction::Gitignore, project.path(), Some(&cache));
        let second = run(CacheAction::Gitignore, project.path(), Some(&cache));

        // assert
        assert!(first.starts_with("Added "));
        assert!(first.ends_with(": /.tf_cache/\n"));
        assert!(second.starts_with("Already in "));
        let gitignore = fs::read_to_string(project.path().join(".gitignore")).unwrap();
        assert_eq!("/.tf_cache/\n", gitignore);
    }

    #[test]
    fn test_should_say_so_when_the_cache_is_the_project_root() {
        // arrange
        let project = tempfile::tempdir().unwrap();
        fs::write(project.path().join("pyproject.toml"), "").unwrap();

        // act
        let report = run(CacheAction::Gitignore, project.path(), Some(project.path()));

        // assert
        assert!(report.contains("not gitignoring the whole project"));
    }

    #[test]
    fn test_should_explain_when_the_cache_is_outside_the_project() {
        // arrange
        let project = tempfile::tempdir().unwrap();
        fs::write(project.path().join("pyproject.toml"), "").unwrap();
        let outside = tempfile::tempdir().unwrap();

        // act
        let report = run(CacheAction::Gitignore, project.path(), Some(outside.path()));

        // assert
        assert!(report.contains("is outside the project, so there is nothing to gitignore."));
    }

    #[cfg(unix)]
    #[test]
    fn test_should_make_cache_files_private_but_leave_a_shared_directory_alone() {
        use std::os::unix::fs::PermissionsExt;
        // arrange
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("index-ab.bin");
        fs::write(&file, b"x").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        let dir_mode_before = fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777;

        // act
        tighten_cache_permissions(dir.path());

        // assert
        assert_eq!(
            0o600,
            fs::metadata(&file).unwrap().permissions().mode() & 0o777
        );
        assert_eq!(
            dir_mode_before,
            fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777
        );
    }
}
