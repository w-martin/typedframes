use super::*;
use ruff_python_parser::parse_module;

mod calls;
mod classes;
mod folded_values;
mod folding;

pub(super) fn s(v: &str) -> Value {
    Value::Str(v.to_string())
}

pub(super) fn strs(items: &[&str]) -> Value {
    Value::List(items.iter().map(|i| s(i)).collect())
}

// Every folded expression as (its source text, its value), in source order.
pub(super) fn folds(source: &str) -> Vec<(String, Value)> {
    let module = parse_module(source).unwrap().into_syntax();
    let mut sites: Vec<(TextRange, Value)> =
        find_sites(&module, &Analysis::build(&module, &ImportedModules::new()))
            .0
            .into_iter()
            .collect();
    sites.sort_by_key(|(range, _)| range.start());
    sites
        .into_iter()
        .map(|(range, value)| (source[range].to_string(), value))
        .collect()
}

// The value `name` folds to where it is used as `print(name)`, if it does.
pub(super) fn used_as(source: &str, name: &str) -> Option<Value> {
    folds(source)
        .into_iter()
        .find(|(text, _)| text == name)
        .map(|(_, v)| v)
}

pub(super) const USE: &str = "\nprint(COLS)\n";

pub(super) fn with_print(prefix: &str) -> String {
    format!("{prefix}{USE}")
}

// The value a whole expression folds to, found by its source text.
pub(super) fn folded(source: &str, text: &str) -> Option<Value> {
    folds(source)
        .into_iter()
        .find(|(t, _)| t == text)
        .map(|(_, v)| v)
}

pub(super) fn folded_value_of_last_statement(source: &str) -> Option<Value> {
    let module = parse_module(source).unwrap().into_syntax();
    let folded = fold_constants(&module, &ImportedModules::new())?;
    let ast::Stmt::Assign(assign) = folded.body.last()? else {
        return None;
    };
    // evaluating the replacement with NO names proves it is a self-contained literal
    Evaluator::new(&NoNames).eval(&assign.value)
}
