//! `pyproject.toml` `[tool.typedframes]` configuration loading and project-root
//! discovery.

use std::fs;
use std::path::{Path, PathBuf};

// Root deserialisation target for `pyproject.toml`.
#[derive(serde::Deserialize)]
struct Config {
    tool: Option<ToolConfig>,
}

// `[tool]` section of `pyproject.toml`.
#[derive(serde::Deserialize)]
struct ToolConfig {
    typedframes: Option<LinterConfig>,
}

// `[tool.typedframes]` configuration block.
// All fields are optional; absent keys default as documented on each field.
#[derive(serde::Deserialize)]
pub struct LinterConfig {
    pub(crate) enabled: Option<bool>,  // default: true
    pub(crate) warnings: Option<bool>, // default: true
    // Dialect name used to fold unquoted SQL identifier case when inferring columns
    // from a literal SELECT list (e.g. "snowflake", "postgres", "bigquery"). Unknown or
    // absent values fall back to `SqlDialect::Generic` (no folding) — see
    // `SqlDialect::from_config_str`.
    pub(crate) sql_dialect: Option<String>,
    // Explicit allowlist of installed (non-project) package names whose own
    // Annotated[...]/BaseSchema declarations and recognized transform patterns should
    // be indexed the same way first-party files are -- e.g. an internal company
    // package that wraps a SQL connector. Resolved from the project's own auto-detected
    // `.venv` site-packages directory (see `find_site_packages_dir`); no editable-install
    // support and no path override in this version. Deliberately an explicit allowlist
    // rather than indexing all of site-packages, which would be both expensive and a
    // much larger, unbounded trust surface.
    pub(crate) trace_external_packages: Option<Vec<String>>,
    // Opt-out counterpart to the default-on candidate tracing (see
    // `discover_external_package_candidates` in index.rs): a package named here is
    // never traced, even if the project's own code calls it in a way that looks
    // DataFrame-shaped. Does NOT affect `trace_external_packages` -- an explicit
    // force-include there always wins, since naming a package in both lists is a
    // contradiction the user should resolve, not one this checker should silently
    // arbitrate.
    pub(crate) excluded_external_packages: Option<Vec<String>>,
    // Directory names to prune when collecting `.py` files. When set, REPLACES the
    // built-in default set (`DEFAULT_EXCLUDED_DIRS` -- `.git`, `.venv`, `node_modules`,
    // `__pycache__`, `.claude`, etc.) entirely rather than adding to it, matching
    // ruff's own `exclude` (as opposed to `extend-exclude`, which this checker doesn't
    // have a separate option for) -- an explicit `exclude = []` means "prune nothing
    // at all", a deliberate override, not "nothing configured". Matches by bare
    // directory name, not a path/glob pattern. The Python CLI's own file collector
    // (`_collect_python_files` in cli.py) reads this same key independently via
    // `tomllib`, so both collectors stay in sync from one config value.
    pub(crate) exclude: Option<Vec<String>>,
}

impl LinterConfig {
    pub(crate) const EMPTY: Self = Self {
        enabled: None,
        warnings: None,
        sql_dialect: None,
        trace_external_packages: None,
        excluded_external_packages: None,
        exclude: None,
    };
}

// Read `[tool.typedframes]` from `pyproject.toml` at `project_root`.
// Returns a config with all fields `None` if the file is absent, unreadable, or has no
// `[tool.typedframes]` section; callers use `.unwrap_or(true)` on each field.
pub(crate) fn load_linter_config(project_root: &Path) -> LinterConfig {
    let config_path = project_root.join("pyproject.toml");
    if !config_path.exists() {
        return LinterConfig::EMPTY;
    }

    let content = match fs::read_to_string(config_path) {
        Ok(c) => c,
        Err(_) => return LinterConfig::EMPTY,
    };

    let config: Config = match toml::from_str(&content) {
        Ok(c) => c,
        Err(_) => return LinterConfig::EMPTY,
    };

    config
        .tool
        .and_then(|t| t.typedframes)
        .unwrap_or(LinterConfig::EMPTY)
}

/// Return `true` if the linter is enabled for `project_root` (default: `true`).
pub fn is_enabled(project_root: &Path) -> bool {
    load_linter_config(project_root).enabled.unwrap_or(true)
}

/// Walk up the directory tree from `start_path` until a `pyproject.toml` is found.
///
/// Returns the directory containing `pyproject.toml`, or `start_path` itself if no
/// `pyproject.toml` exists anywhere in the ancestor chain (e.g. standalone scripts).
pub fn find_project_root(start_path: &Path) -> PathBuf {
    let mut current = start_path.to_path_buf();
    if current.is_file() {
        current.pop();
    }
    loop {
        if current.join("pyproject.toml").exists() {
            return current;
        }
        if !current.pop() {
            return start_path.to_path_buf();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_project_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let sub = root.join("a/b/c");
        fs::create_dir_all(&sub).unwrap();
        fs::write(root.join("pyproject.toml"), "").unwrap();

        assert_eq!(find_project_root(&sub), root);
        assert_eq!(find_project_root(root), root);
    }

    #[test]
    fn test_is_enabled() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();

        // Case 1: No pyproject.toml -> enabled by default
        assert!(is_enabled(root));

        // Case 2: pyproject.toml without tool section -> enabled by default
        fs::write(
            root.join("pyproject.toml"),
            "[tool.something]\nenabled = false",
        )
        .unwrap();
        assert!(is_enabled(root));

        // Case 3: pyproject.toml with tool.typedframes.enabled = false
        fs::write(
            root.join("pyproject.toml"),
            "[tool.typedframes]\nenabled = false",
        )
        .unwrap();
        assert!(!is_enabled(root));

        // Case 4: pyproject.toml with tool.typedframes.enabled = true
        fs::write(
            root.join("pyproject.toml"),
            "[tool.typedframes]\nenabled = true",
        )
        .unwrap();
        assert!(is_enabled(root));
    }

    #[test]
    fn test_load_linter_config_reads_sql_dialect() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();

        // Absent entirely -> None, so `with_context` leaves the dialect at its
        // Generic default rather than overwriting it.
        fs::write(
            root.join("pyproject.toml"),
            "[tool.typedframes]\nenabled = true",
        )
        .unwrap();
        assert_eq!(load_linter_config(root).sql_dialect, None);

        fs::write(
            root.join("pyproject.toml"),
            "[tool.typedframes]\nsql_dialect = \"snowflake\"",
        )
        .unwrap();
        assert_eq!(
            load_linter_config(root).sql_dialect,
            Some("snowflake".to_string())
        );
    }

    #[test]
    fn test_load_linter_config_reads_exclude_list() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();

        fs::write(
            root.join("pyproject.toml"),
            "[tool.typedframes]\nexclude = [\".claude\", \"vendor\"]",
        )
        .unwrap();
        assert_eq!(
            load_linter_config(root).exclude,
            Some(vec![".claude".to_string(), "vendor".to_string()])
        );
    }

    #[test]
    fn test_load_linter_config_reads_excluded_external_packages() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();

        fs::write(
            root.join("pyproject.toml"),
            "[tool.typedframes]\nenabled = true",
        )
        .unwrap();
        assert_eq!(load_linter_config(root).excluded_external_packages, None);

        fs::write(
            root.join("pyproject.toml"),
            "[tool.typedframes]\nexcluded_external_packages = [\"huge_untrusted_pkg\"]",
        )
        .unwrap();
        assert_eq!(
            load_linter_config(root).excluded_external_packages,
            Some(vec!["huge_untrusted_pkg".to_string()])
        );
    }
}
