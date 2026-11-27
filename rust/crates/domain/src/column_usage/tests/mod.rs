use super::*;
use ruff_python_parser::parse_module;

mod derivations;
mod nested_mentions;
mod other_uses;
mod writes;

pub(super) fn usage(source: &str) -> ColumnUsage {
    let module = parse_module(source).unwrap().into_syntax();
    scan_usage("df", &module.body, &|_| 0)
}

pub(super) fn cols(usage: &ColumnUsage) -> Vec<(&str, bool)> {
    usage
        .accesses
        .iter()
        .map(|a| (a.column.as_str(), a.conditional))
        .collect()
}

pub(super) fn others(usage: &ColumnUsage) -> Vec<(&str, bool, Consumption)> {
    usage
        .other_uses
        .iter()
        .map(|u| (u.description.as_str(), u.conditional, u.consumes))
        .collect()
}

pub(super) fn has_other(u: &ColumnUsage, description: &str) -> bool {
    u.other_uses.iter().any(|o| o.description == description)
}

pub(super) fn mention(source: &str) -> bool {
    let module = parse_module(source).unwrap().into_syntax();
    first_nested_scope_mention(&module.body, &HashSet::from(["df".to_string()])).is_some()
}

pub(super) fn declares(source: &str) -> bool {
    let module = parse_module(source).unwrap().into_syntax();
    declares_global_or_nonlocal(&module.body, "df")
}
