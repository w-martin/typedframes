use super::*;

/// The columns a `drop(..)` call certainly removes. Positional labels without `axis=` are
/// columns only in polars (`polars`); pandas reads them as row labels.
pub(crate) fn extract_drop_columns(call: &ast::ExprCall, polars: bool) -> Option<Vec<String>> {
    // `level=` drops from one level of a MultiIndex: the flat name may survive.
    if call
        .arguments
        .keywords
        .iter()
        .any(|k| k.arg.as_ref().is_some_and(|a| a.as_str() == "level"))
    {
        return None;
    }
    // Check `columns=` kwarg first (pandas pattern — always correct for column drops)
    for keyword in &call.arguments.keywords {
        if keyword.arg.as_ref().map(|s| s.as_str()) == Some("columns") {
            return extract_string_list_or_single(&keyword.value);
        }
    }

    // Check for axis kwarg
    let axis_kwarg = call
        .arguments
        .keywords
        .iter()
        .find(|k| k.arg.as_ref().map(|s| s.as_str()) == Some("axis"));

    if let Some(axis_kw) = axis_kwarg {
        // axis kwarg present — only drop columns when axis=1
        if let Some(value) = drop_columns_on_axis_one(call, axis_kw) {
            return value;
        }
        return None; // axis present but not 1 → row drop
    }

    if !polars {
        return None;
    }
    // `drop("a", "b")` and `drop(["a"], "b")` both name columns.
    let mut dropped = Vec::new();
    for arg in &call.arguments.args {
        dropped.extend(extract_string_list_or_single(arg)?);
    }
    (!dropped.is_empty()).then_some(dropped)
}

fn drop_columns_on_axis_one(
    call: &ruff_python_ast::ExprCall,
    axis_kw: &ruff_python_ast::Keyword,
) -> Option<Option<Vec<String>>> {
    if let Expr::NumberLiteral(n) = &axis_kw.value {
        if let ast::Number::Int(ref i) = n.value {
            if i.as_u64() == Some(1) {
                if let Some(first_arg) = call.arguments.args.first() {
                    return Some(extract_string_list_or_single(first_arg));
                }
            }
        }
    }
    None
}

// Extract rename mapping from a rename() call: {"old": "new", ...}.
// The string keys of a dict literal whose keys are all string literals.
pub(crate) fn extract_string_dict_keys(dict: &ast::ExprDict) -> Option<Vec<String>> {
    dict.items
        .iter()
        .map(|item| match item.key.as_ref()? {
            Expr::StringLiteral(s) => Some(s.value.to_str().to_string()),
            _ => None,
        })
        .collect()
}

pub(crate) fn extract_rename_mapping(call: &ast::ExprCall) -> Option<HashMap<String, String>> {
    // Check `columns={"old": "new"}` kwarg (pandas)
    for keyword in &call.arguments.keywords {
        if keyword.arg.as_ref().map(|s| s.as_str()) == Some("columns") {
            if let Expr::Dict(dict) = &keyword.value {
                return extract_string_dict(dict);
            }
        }
    }
    // Fall back to first positional arg dict (polars)
    if let Some(Expr::Dict(dict)) = call.arguments.args.first() {
        return extract_string_dict(dict);
    }
    None
}

// `str.lower` / `str.upper` referenced bare (not called) — the shape `rename(...)`
// takes as its `columns=`/first-positional argument to case-fold every column,
// rather than remap specific ones by name.
pub(crate) fn expr_as_str_case_fold(expr: &Expr) -> Option<CaseFold> {
    if let Expr::Attribute(attr) = expr {
        if let Expr::Name(name) = &*attr.value {
            if name.id.as_str() == "str" {
                return match attr.attr.as_str() {
                    "lower" => Some(CaseFold::Lower),
                    "upper" => Some(CaseFold::Upper),
                    _ => None,
                };
            }
        }
    }
    None
}

// Extract a case-fold from a rename() call: `.rename(columns=str.lower)` (pandas)
// or `.rename(str.lower)` (polars' first-positional convention, mirroring
// extract_rename_mapping's dict handling above).
pub(crate) fn extract_rename_case_fold(call: &ast::ExprCall) -> Option<CaseFold> {
    for keyword in &call.arguments.keywords {
        if keyword.arg.as_ref().map(|s| s.as_str()) == Some("columns") {
            return expr_as_str_case_fold(&keyword.value);
        }
    }
    if let Some(first_arg) = call.arguments.args.first() {
        return expr_as_str_case_fold(first_arg);
    }
    None
}

// `df.columns.str.lower()` / `.str.upper()` — the RHS shape of
// `df.columns = df.columns.str.lower()`. Returns the receiver's variable name
// (so the caller can confirm it matches the assignment target) and the fold.
pub(crate) fn extract_columns_str_fold(value: &Expr) -> Option<(String, CaseFold)> {
    let Expr::Call(call) = value else {
        return None;
    };
    let Expr::Attribute(fold_attr) = &*call.func else {
        return None;
    };
    let fold = match fold_attr.attr.as_str() {
        "lower" => CaseFold::Lower,
        "upper" => CaseFold::Upper,
        _ => return None,
    };
    let Expr::Attribute(str_attr) = &*fold_attr.value else {
        return None;
    };
    if str_attr.attr.as_str() != "str" {
        return None;
    }
    let Expr::Attribute(columns_attr) = &*str_attr.value else {
        return None;
    };
    if columns_attr.attr.as_str() != "columns" {
        return None;
    }
    let Expr::Name(recv) = &*columns_attr.value else {
        return None;
    };
    Some((recv.id.to_string(), fold))
}

pub(crate) fn extract_string_dict(dict: &ast::ExprDict) -> Option<HashMap<String, String>> {
    let mut map = HashMap::new();
    for item in &dict.items {
        if let Some(key) = &item.key {
            match (
                extract_string_literal(key),
                extract_string_literal(&item.value),
            ) {
                (Some(k), Some(v)) => {
                    map.insert(k.to_string(), v.to_string());
                }
                _ => return None, // Non-literal key or value
            }
        }
    }
    Some(map)
}
