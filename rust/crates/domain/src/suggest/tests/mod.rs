use super::*;
use crate::column_usage::scan_usage;
use ruff_python_ast::Stmt;
use ruff_python_parser::parse_module;

mod blockers;
mod final_annotations;
mod library;
mod selection;

// The imports every test source is assumed to have at the top.
pub(super) fn test_libraries() -> LibraryContext {
    let imports = parse_module("import pandas as pd\nimport polars as pl\n")
        .unwrap()
        .into_syntax();
    LibraryContext::new(&imports.body, &HashMap::new(), &HashMap::new())
}

// Runs the rule on the first assignment of `source`, scanning the rest of the module.
pub(super) fn run(source: &str, function_local: bool) -> Result<Suggestion, String> {
    let module = parse_module(source).unwrap().into_syntax();
    let first = module
        .body
        .iter()
        .position(|stmt| matches!(stmt, Stmt::Assign(_)))
        .expect("source must contain an assignment");
    let Stmt::Assign(assign) = &module.body[first] else {
        unreachable!()
    };
    let Expr::Name(target) = &assign.targets[0] else {
        panic!("target must be a name");
    };
    let usage = scan_usage(target.id.as_str(), &module.body[first + 1..], &|offset| {
        source[..offset.to_usize()].matches('\n').count() + 1
    });
    let mut libraries = test_libraries();
    libraries.extend(&module.body);
    suggest_selection(
        &Origin {
            libraries: &libraries,
            annotation: None,
            source,
            display_path: "f.py",
            value: Some(&assign.value),
            var: target.id.as_str(),
            line: 1,
            col: 1,
            stmt_start: assign.range().start().to_usize(),
            stmt_end: assign.range().end().to_usize(),
            function_local,
            receivers: &HashMap::new(),
            known_columns: &[],
            scope_body: &module.body,
        },
        &usage,
    )
}

pub(super) fn applied(source: &str, suggestion: &Suggestion) -> String {
    let edit = &suggestion.edits[0];
    format!(
        "{}{}{}",
        &source[..edit.start],
        edit.replacement,
        &source[edit.end..]
    )
}

pub(super) fn selection_of(source: &str) -> String {
    run(source, true).unwrap().edits[0].replacement.clone()
}

pub(super) fn final_fix(
    source: &str,
    class: &str,
    attr: &str,
) -> Result<(Suggestion, String), String> {
    let module = parse_module(source).unwrap().into_syntax();
    let suggestion = suggest_final_constant(&module, source, "f.py", class, attr)?;
    let edited = crate::edits::apply_edits(source, &suggestion.edits).unwrap();
    Ok((suggestion, edited))
}
