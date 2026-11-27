use super::*;
use std::path::PathBuf;

mod explain;

pub(super) fn tallies(entries: &[(&str, u64, u64)]) -> BTreeMap<String, Tally> {
    entries
        .iter()
        .map(|(name, typed, total)| {
            (
                name.to_string(),
                Tally {
                    total: *total,
                    typed: *typed,
                },
            )
        })
        .collect()
}

pub(super) fn config(fail_under: f64, overrides: &[(&str, f64)]) -> CoverageConfig {
    CoverageConfig {
        enabled: true,
        fail_under,
        overrides: overrides.iter().map(|(p, t)| (p.to_string(), *t)).collect(),
        ..CoverageConfig::default()
    }
}

pub(super) fn explain_sources_fixture() -> (Vec<Value>, Vec<Value>, Vec<Value>) {
    let calls = vec![
        json!({"file": "/p/a.py", "line": 2, "col": 5, "label": "pd.read_csv", "context": "assignment", "phrase": null}),
        json!({"file": "/p/a.py", "line": 7, "col": 5, "label": "pd.read_csv", "context": "a bare return"}),
        json!({"file": "/p/a.py", "line": 9, "col": 5, "label": "pd.read_json", "context": "x"}),
    ];
    let typed = vec![json!({"file": "/p/a.py", "line": 2, "schema": "Orders"})];
    let untyped =
        vec![json!({"file": "/p/a.py", "line": 9, "col": 1, "var": "df", "phrase": "quiet-amber"})];
    (calls, typed, untyped)
}
