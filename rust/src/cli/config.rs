//! `[tool.typedframes]` settings the command line reads itself: which directories to skip while
//! collecting files, and the opt-in DataFrame schema coverage gate.

use super::pyfmt::{float_str, repr_str};
use std::collections::HashSet;
use std::fs;
use std::path::Path;

const DEFAULT_FAIL_UNDER: f64 = 100.0;
const PCT_MAX: f64 = 100.0;

/// How much coverage detail to print.
#[derive(Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum)]
pub(crate) enum CoverageDetail {
    Summary,
    TermMissing,
    Explain,
}

impl CoverageDetail {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "summary" => Some(Self::Summary),
            "term-missing" => Some(Self::TermMissing),
            "explain" => Some(Self::Explain),
            _ => None,
        }
    }
}

/// Resolved coverage threshold settings for one `check` run. The default is "nothing
/// configured": disabled, so no threshold is evaluated at all.
#[derive(Clone, Debug)]
pub(crate) struct CoverageConfig {
    pub(crate) enabled: bool,
    pub(crate) fail_under: f64,
    pub(crate) overrides: Vec<(String, f64)>,
    pub(crate) detail: CoverageDetail,
}

impl Default for CoverageConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            fail_under: DEFAULT_FAIL_UNDER,
            overrides: Vec::new(),
            detail: CoverageDetail::Summary,
        }
    }
}

// Bad config is reported rather than silently defaulted, on stderr to keep stdout clean: a
// threshold the user believes is enforced but isn't is worse than a noisy run.
fn warn(message: &str) {
    eprintln!("typedframes: {message}");
}

// What Python's `type(value).__name__` called a TOML value.
fn type_name(value: &toml::Value) -> &'static str {
    match value {
        toml::Value::String(_) => "str",
        toml::Value::Integer(_) => "int",
        toml::Value::Float(_) => "float",
        toml::Value::Boolean(_) => "bool",
        toml::Value::Array(_) => "list",
        toml::Value::Table(_) => "dict",
        toml::Value::Datetime(datetime) => match (datetime.date.is_some(), datetime.time.is_some())
        {
            (true, true) => "datetime",
            (true, false) => "date",
            _ => "time",
        },
    }
}

fn parse_threshold(value: &toml::Value, label: &str) -> Option<f64> {
    let pct = match value {
        toml::Value::Integer(n) => *n as f64,
        toml::Value::Float(f) => *f,
        other => {
            warn(&format!(
                "ignoring {label}: expected a number, got {}",
                type_name(other)
            ));
            return None;
        }
    };
    if !(0.0..=PCT_MAX).contains(&pct) {
        warn(&format!(
            "ignoring {label}: {} is outside the 0-100 range",
            float_str(pct)
        ));
        return None;
    }
    Some(pct)
}

fn line_and_column(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..offset.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, column)
}

// A nested table out of a TOML file, or `None` if it isn't there. A missing file is silent
// (nothing was configured); a malformed one warns, since the user clearly meant to configure
// something.
fn read_toml_table(config_path: &Path, keys: &[&str]) -> Option<toml::Table> {
    if !config_path.is_file() {
        return None;
    }
    let text = match fs::read(config_path)
        .map_err(|e| e.to_string())
        .and_then(|bytes| String::from_utf8(bytes).map_err(|e| e.to_string()))
    {
        Ok(text) => text,
        Err(error) => {
            warn(&format!("ignoring {}: {error}", config_path.display()));
            return None;
        }
    };
    let data: toml::Table = match text.parse() {
        Ok(data) => data,
        Err(error) => {
            let at = error.span().map_or(String::new(), |span| {
                let (line, column) = line_and_column(&text, span.start);
                format!(" (at line {line}, column {column})")
            });
            warn(&format!(
                "ignoring {}: {}{at}",
                config_path.display(),
                error.message()
            ));
            return None;
        }
    };
    let mut table = toml::Value::Table(data);
    for key in keys {
        table = table.as_table()?.get(*key)?.clone();
    }
    table.try_into().ok()
}

fn coverage_config_from_table(table: &toml::Table) -> CoverageConfig {
    let enabled = match table.get("enabled") {
        None => false,
        Some(toml::Value::Boolean(flag)) => *flag,
        Some(_) => {
            warn("ignoring coverage.enabled: expected true or false");
            false
        }
    };

    let mut fail_under = DEFAULT_FAIL_UNDER;
    if let Some(value) = table.get("fail_under") {
        if let Some(parsed) = parse_threshold(value, "coverage.fail_under") {
            fail_under = parsed;
        }
    }

    let detail = match table.get("detail") {
        None => CoverageDetail::Summary,
        Some(value) => value
            .as_str()
            .and_then(CoverageDetail::from_name)
            .unwrap_or_else(|| {
                warn("ignoring coverage.detail: expected one of summary, term-missing, explain");
                CoverageDetail::Summary
            }),
    };

    let mut overrides = Vec::new();
    match table.get("overrides") {
        None => {}
        Some(toml::Value::Table(raw)) => {
            for (pattern, value) in raw {
                let label = format!("coverage.overrides[{}]", repr_str(pattern));
                if let Some(parsed) = parse_threshold(value, &label) {
                    overrides.push((pattern.clone(), parsed));
                }
            }
        }
        // Present with a non-table value, `overrides = []` included: a type mistake worth
        // reporting rather than silently treating as "none set".
        Some(_) => warn("ignoring coverage.overrides: expected a table of glob = threshold"),
    }

    CoverageConfig {
        enabled,
        fail_under,
        overrides,
        detail,
    }
}

/// The opt-in coverage settings for the project at `path`.
///
/// A standalone `typedframes.toml` at the project root wins entirely over `[tool.typedframes]` in
/// `pyproject.toml` (the two are never merged); in `typedframes.toml` the `[tool.typedframes]`
/// prefix is dropped, so the table is `[coverage]`. Only `path` itself is looked at, and only when
/// it is a directory: checking a single file never picks up a coverage config file. Anything
/// unusable gives the disabled default, which keeps the opt-in guarantee: no table, no threshold.
pub(crate) fn load_coverage_config(path: &Path) -> CoverageConfig {
    if !path.is_dir() {
        return CoverageConfig::default();
    }
    let standalone = path.join("typedframes.toml");
    let table = if standalone.is_file() {
        read_toml_table(&standalone, &["coverage"])
    } else {
        read_toml_table(
            &path.join("pyproject.toml"),
            &["tool", "typedframes", "coverage"],
        )
    };
    table.map_or_else(CoverageConfig::default, |t| coverage_config_from_table(&t))
}

/// `exclude` from `[tool.typedframes]` in `path/pyproject.toml`: the directory names to skip.
///
/// `None` means nothing is configured (no file, no section, no key, or a malformed value): use
/// the built-in set. An explicitly empty list is not `None`: it means skip nothing. Only `path`
/// itself is looked at, and only when it is a directory.
pub(crate) fn load_configured_excludes(path: &Path) -> Option<HashSet<String>> {
    if !path.is_dir() {
        return None;
    }
    let text = fs::read_to_string(path.join("pyproject.toml")).ok()?;
    let data: toml::Table = text.parse().ok()?;
    let exclude = data
        .get("tool")?
        .get("typedframes")?
        .get("exclude")?
        .as_array()?;
    Some(
        exclude
            .iter()
            .filter_map(|entry| entry.as_str().map(str::to_string))
            .collect(),
    )
}

/// One step of a compiled glob.
enum Step {
    Literal(char),
    /// `?`: one character other than `/`.
    One,
    /// `*`: any run of characters other than `/`.
    Segment,
    /// `**`: any run of characters, line breaks aside.
    Any,
    /// Front of `**/`: the two steps after it may be skipped, so `**/x.py` still matches a bare `x.py`.
    Optional,
}

/// A project-relative path glob, matched against the whole path: `**` spans any number of path
/// segments (`**/` consumes its separator too), `*` and `?` never cross a `/`, everything else is
/// literal.
pub(crate) struct Glob {
    steps: Vec<Step>,
}

impl Glob {
    pub(crate) fn new(pattern: &str) -> Self {
        let chars: Vec<char> = pattern.chars().collect();
        let mut steps = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            match chars[i] {
                '*' => {
                    let mut end = i;
                    while chars.get(end) == Some(&'*') {
                        end += 1;
                    }
                    if end - i < 2 {
                        steps.push(Step::Segment);
                    } else if chars.get(end) == Some(&'/') {
                        steps.extend([Step::Optional, Step::Any, Step::Literal('/')]);
                        end += 1;
                    } else {
                        steps.push(Step::Any);
                    }
                    i = end;
                }
                '?' => {
                    steps.push(Step::One);
                    i += 1;
                }
                ch => {
                    steps.push(Step::Literal(ch));
                    i += 1;
                }
            }
        }
        Self { steps }
    }

    /// Runs every live position through the path at once, so a pattern with several `**` cannot
    /// backtrack exponentially.
    pub(crate) fn is_match(&self, path: &str) -> bool {
        let mut live = vec![false; self.steps.len() + 1];
        self.enter(&mut live, 0);
        for ch in path.chars() {
            let mut next = vec![false; live.len()];
            for (at, step) in self.steps.iter().enumerate() {
                if !live[at] {
                    continue;
                }
                match step {
                    Step::Literal(expected) if *expected == ch => self.enter(&mut next, at + 1),
                    Step::One if ch != '/' => self.enter(&mut next, at + 1),
                    Step::Segment if ch != '/' => self.enter(&mut next, at),
                    Step::Any if ch != '\n' => self.enter(&mut next, at),
                    _ => {}
                }
            }
            live = next;
        }
        live[self.steps.len()]
    }

    fn enter(&self, live: &mut [bool], at: usize) {
        if std::mem::replace(&mut live[at], true) {
            return;
        }
        match self.steps.get(at) {
            Some(Step::Segment | Step::Any) => self.enter(live, at + 1),
            Some(Step::Optional) => {
                self.enter(live, at + 1);
                self.enter(live, at + 3);
            }
            _ => {}
        }
    }
}

/// How specific an override pattern is, for most-specific-match-wins: the length of the literal
/// prefix before the first glob metacharacter, then the total length, then the pattern itself so
/// ties resolve deterministically.
pub(crate) fn pattern_specificity(pattern: &str) -> (usize, usize, &str) {
    let prefix_end = pattern.find(['*', '?']).unwrap_or(pattern.len());
    (
        pattern[..prefix_end].chars().count(),
        pattern.chars().count(),
        pattern,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(pattern: &str, path: &str) -> bool {
        Glob::new(pattern).is_match(path)
    }

    #[test]
    fn test_should_let_double_star_span_directories() {
        assert!(matches("legacy/**", "legacy/etl/load.py"));
        assert!(matches("legacy/**", "legacy/x.py"));
        assert!(!matches("legacy/**", "other/legacy/x.py"));
    }

    #[test]
    fn test_should_match_a_root_file_with_a_leading_double_star_directory() {
        assert!(matches("**/x.py", "x.py"));
        assert!(matches("**/x.py", "a/b/x.py"));
    }

    #[test]
    fn test_should_keep_single_star_and_question_mark_inside_one_segment() {
        assert!(matches("src/*.py", "src/a.py"));
        assert!(!matches("src/*.py", "src/sub/a.py"));
        assert!(matches("src/?.py", "src/a.py"));
        assert!(!matches("src/?.py", "src/ab.py"));
    }

    #[test]
    fn test_should_treat_regex_characters_in_a_pattern_literally() {
        assert!(matches("a+b/(c).py", "a+b/(c).py"));
        assert!(!matches("a.py", "abpy"));
    }

    #[test]
    fn test_should_match_a_trailing_double_star_directory_only_with_its_separator() {
        assert!(matches("src/**/", "src/a/"));
        assert!(matches("src/**/", "src/"));
        assert!(!matches("src/**/", "src"));
    }

    #[test]
    fn test_should_treat_three_stars_as_a_double_star() {
        assert!(matches("a/***", "a/b/c.py"));
        assert!(matches("a/***/c.py", "a/c.py"));
    }

    #[test]
    fn test_should_match_the_whole_path_not_a_part_of_it() {
        assert!(!matches("a", "ab"));
        assert!(!matches("a", "ba"));
        assert!(matches("", ""));
        assert!(!matches("", "a"));
    }

    #[test]
    fn test_should_not_let_double_star_cross_a_line_break() {
        assert!(!matches("a/**", "a/b\nc"));
        assert!(matches("a/*", "a/b\nc"));
    }

    #[test]
    fn test_should_count_characters_not_bytes_for_a_question_mark() {
        assert!(matches("src/?.py", "src/\u{e9}.py"));
    }

    #[test]
    fn test_should_match_many_double_stars_in_linear_time() {
        let pattern = "**/".repeat(40) + "x.py";
        let path = "a/".repeat(200) + "y.py";
        assert!(!matches(&pattern, &path));
        assert!(matches(&pattern, &("a/".repeat(200) + "x.py")));
    }

    #[test]
    fn test_should_rank_a_longer_literal_prefix_as_more_specific() {
        assert!(pattern_specificity("src/new_module/**") > pattern_specificity("src/**"));
        assert!(pattern_specificity("src/**/a") > pattern_specificity("src/**"));
    }

    #[test]
    fn test_should_read_the_coverage_table_from_pyproject() {
        // arrange
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("pyproject.toml"),
            "[tool.typedframes.coverage]\nenabled = true\nfail_under = 80\ndetail = \"explain\"\n\
             [tool.typedframes.coverage.overrides]\n\"legacy/**\" = 50\n",
        )
        .unwrap();

        // act
        let config = load_coverage_config(dir.path());

        // assert
        assert!(config.enabled);
        assert_eq!(80.0, config.fail_under);
        assert_eq!(CoverageDetail::Explain, config.detail);
        assert_eq!(vec![("legacy/**".to_string(), 50.0)], config.overrides);
    }

    #[test]
    fn test_should_prefer_a_standalone_file_over_pyproject_entirely() {
        // arrange
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("pyproject.toml"),
            "[tool.typedframes.coverage]\nenabled = true\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("typedframes.toml"),
            "[coverage]\nfail_under = 10\n",
        )
        .unwrap();

        // act
        let config = load_coverage_config(dir.path());

        // assert
        assert!(!config.enabled);
        assert_eq!(10.0, config.fail_under);
    }

    #[test]
    fn test_should_give_the_disabled_default_for_a_single_file() {
        // arrange
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.py");
        std::fs::write(&file, "x = 1\n").unwrap();

        // act
        let config = load_coverage_config(&file);

        // assert
        assert!(!config.enabled);
    }

    #[test]
    fn test_should_tell_no_exclude_key_from_an_empty_one() {
        // arrange
        let none = tempfile::tempdir().unwrap();
        std::fs::write(none.path().join("pyproject.toml"), "[tool.typedframes]\n").unwrap();
        let empty = tempfile::tempdir().unwrap();
        std::fs::write(
            empty.path().join("pyproject.toml"),
            "[tool.typedframes]\nexclude = []\n",
        )
        .unwrap();
        let some = tempfile::tempdir().unwrap();
        std::fs::write(
            some.path().join("pyproject.toml"),
            "[tool.typedframes]\nexclude = [\"legacy\", 3]\n",
        )
        .unwrap();

        // act
        let (a, b, c) = (
            load_configured_excludes(none.path()),
            load_configured_excludes(empty.path()),
            load_configured_excludes(some.path()),
        );

        // assert
        assert_eq!(None, a);
        assert_eq!(Some(HashSet::new()), b);
        assert_eq!(Some(HashSet::from(["legacy".to_string()])), c);
    }
}
