//! Stateless `ruff_python_ast` query and extraction helpers.
//!
//! Everything here is a pure function of the AST nodes handed to it: schema-base
//! detection, string/list/dict literal extraction, annotation parsing, ORM column
//! discovery, and `pl.col()` name collection. None of it touches linter state.

use crate::constants::SQL_PRODUCING_METHODS;
use crate::linter::CaseFold;
use ruff_python_ast::{self as ast, Expr, Stmt};
use std::collections::HashMap;

mod annotation;
mod drop_rename;
mod orm;
mod polars;

pub(crate) use annotation::*;
pub(crate) use drop_rename::*;
pub(crate) use orm::*;
pub(crate) use polars::*;

pub(crate) fn is_schema_base(name: &str) -> bool {
    matches!(
        name,
        "BaseSchema" | "DataFrameModel" | "DataFrame" | "BaseFrame"
    )
}

// The bare name of a decorator expression: `staticmethod` for `@staticmethod`,
// `wraps` for both `@wraps(f)` and `@functools.wraps(f)`. `None` for anything else
// (an attribute chain deeper than one level, a subscript, a call with no simple
// name, ...) -- callers treat that the same as an unrecognized name.
pub(crate) fn decorator_name(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Name(name) => Some(name.id.as_str()),
        Expr::Attribute(attr) => Some(attr.attr.as_str()),
        Expr::Call(call) => decorator_name(&call.func),
        _ => None,
    }
}

// Column names for a dict literal used as column data -- `{"a": [...], "b": [...]}`.
// Every item must have a string-literal key: a `**spread` or a computed key means the
// literal doesn't name every column, so claiming an exact set would be wrong.
pub(crate) fn extract_dict_literal_keys(dict: &ast::ExprDict) -> Option<Vec<String>> {
    if dict.items.is_empty() {
        return None;
    }
    let mut columns: Vec<String> = Vec::new();
    for item in &dict.items {
        let key = item.key.as_ref().and_then(|k| extract_string_literal(k))?;
        if !columns.iter().any(|c| c == key) {
            columns.push(key.to_string());
        }
    }
    Some(columns)
}

// Column names for `pd.DataFrame([{...}, {...}])` -- a literal list of per-row dict
// literals (pandas' "records" orientation). Every element must be a dict literal with
// only string-literal keys; the result is the union of every row's keys, in
// first-seen order (matching how pandas itself orders the resulting columns) -- a row
// missing a key another row has doesn't invalidate the inference, it's just NaN for
// that row.
pub(crate) fn extract_records_list_columns(list: &ast::ExprList) -> Option<Vec<String>> {
    if list.elts.is_empty() {
        return None;
    }
    let mut columns = Vec::new();
    for elt in &list.elts {
        let Expr::Dict(dict) = elt else {
            return None;
        };
        for item in &dict.items {
            let key = item.key.as_ref().and_then(|k| extract_string_literal(k))?;
            if !columns.iter().any(|c: &String| c == key) {
                columns.push(key.to_string());
            }
        }
    }
    Some(columns)
}

pub(crate) fn extract_string_literal(expr: &Expr) -> Option<&str> {
    if let Expr::StringLiteral(s) = expr {
        Some(s.value.to_str())
    } else {
        None
    }
}

// Extract a list of string literals from a `["a", "b", ...]` list expression.
// Returns None if the expression is not a list or any element is not a string literal.
pub(crate) fn extract_string_list(expr: &Expr) -> Option<Vec<String>> {
    let elements = match expr {
        Expr::List(list) => &list.elts,
        Expr::Tuple(tuple) => &tuple.elts,
        _ => return None,
    };
    let mut result = Vec::new();
    for el in elements {
        if let Expr::StringLiteral(s) = el {
            result.push(s.value.to_str().to_string());
        } else {
            return None;
        }
    }
    Some(result)
}

// Extract columns from a list or single string expression.
pub(crate) fn extract_string_list_or_single(expr: &Expr) -> Option<Vec<String>> {
    match expr {
        Expr::List(_) | Expr::Tuple(_) => extract_string_list(expr),
        Expr::StringLiteral(s) => Some(vec![s.value.to_str().to_string()]),
        _ => None,
    }
}

pub(crate) fn feast_columns_from_list_expr(
    features_list: &Expr,
    full_feature_names: bool,
) -> Option<Vec<String>> {
    let Expr::List(list) = features_list else {
        return None;
    };
    let mut raw = Vec::with_capacity(list.elts.len());
    for elt in &list.elts {
        raw.push(extract_string_literal(elt)?.to_string());
    }
    feast_columns_from_raw_items(&raw, full_feature_names)
}

// The "view:feature" splitting and full_feature_names formatting shared by
// feast_columns_from_list_expr (a literal AST list) and eval_feast_string_expr
// (raw strings recovered by evaluating a callee's `return [...]`, possibly with
// call-site arguments substituted in — see eval_feast_call).
pub(crate) fn feast_columns_from_raw_items(
    raw: &[String],
    full_feature_names: bool,
) -> Option<Vec<String>> {
    let mut columns = Vec::with_capacity(raw.len());
    for literal in raw {
        let (view, feature) = literal.split_once(':')?;
        // `feature` still contains any colon after the first (split_once only
        // splits on the first match), so this rejects anything but exactly one `:`
        // in the whole ref.
        if view.is_empty() || feature.is_empty() || feature.contains(':') {
            return None;
        }
        columns.push(if full_feature_names {
            format!("{view}__{feature}")
        } else {
            feature.to_string()
        });
    }
    Some(columns)
}

pub(crate) fn sql_producing_call(expr: &Expr) -> Option<&ast::ExprCall> {
    let Expr::Call(call) = expr else {
        return None;
    };
    let fn_name = match &*call.func {
        Expr::Name(n) => n.id.as_str(),
        Expr::Attribute(a) => a.attr.as_str(),
        _ => return None,
    };
    SQL_PRODUCING_METHODS.contains(&fn_name).then_some(call)
}

pub(crate) fn is_file_parent_expr(expr: &Expr) -> bool {
    let Expr::Attribute(attr) = expr else {
        return false;
    };
    if attr.attr.as_str() != "parent" {
        return false;
    }
    let Expr::Call(call) = &*attr.value else {
        return false;
    };
    let is_path_ctor = match &*call.func {
        Expr::Name(n) => n.id.as_str() == "Path",
        Expr::Attribute(a) => a.attr.as_str() == "Path",
        _ => false,
    };
    is_path_ctor
        && matches!(
            call.arguments.args.first(),
            Some(Expr::Name(n)) if n.id.as_str() == "__file__"
        )
}

#[cfg(test)]
mod tests;
