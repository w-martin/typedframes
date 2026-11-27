use super::*;

// Check if a type name is a DataFrame/Frame type
pub(crate) fn is_frame_type(name: &str) -> bool {
    name == "DataFrame"
}

// Extract schema name from a type annotation like DataFrame[Schema] or Annotated[pd.DataFrame, Schema]
pub(crate) fn extract_schema_from_annotation(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Subscript(subscript) => extract_schema_from_subscript(subscript),
        Expr::StringLiteral(s) => quoted_dataframe_schema(s),
        _ => None,
    }
}

fn extract_schema_from_subscript(subscript: &ruff_python_ast::ExprSubscript) -> Option<&str> {
    let type_name = match &*subscript.value {
        Expr::Name(name) => Some(name.id.as_str()),
        Expr::Attribute(attr) => Some(attr.attr.as_str()),
        _ => None,
    };
    if let Some(name) = type_name {
        if is_frame_type(name) {
            if let Expr::Name(schema_name) = &*subscript.slice {
                return Some(schema_name.id.as_str());
            }
        }
        // Handle Annotated[pd.DataFrame, Schema] — schema is second tuple element
        if name == "Annotated" {
            if let Some(value) = annotated_schema_name(subscript) {
                return value;
            }
        }
    }
    None
}

fn annotated_schema_name(subscript: &ruff_python_ast::ExprSubscript) -> Option<Option<&str>> {
    if let Expr::Tuple(tuple) = &*subscript.slice {
        if tuple.elts.len() >= 2 {
            if let Expr::Name(schema_name) = &tuple.elts[1] {
                return Some(Some(schema_name.id.as_str()));
            }
        }
    }
    None
}

fn quoted_dataframe_schema(s: &ruff_python_ast::ExprStringLiteral) -> Option<&str> {
    let text = s.value.to_str();
    if text.contains("DataFrame[") {
        if let Some(start) = text.find('[') {
            if let Some(end) = text.rfind(']') {
                let schema = text[start + 1..end].trim();
                if !schema.is_empty() && !schema.contains(',') {
                    return Some(schema);
                }
            }
        }
    }
    None
}

// Recognize a BARE `pd.DataFrame` / `pl.DataFrame` (or a bare-imported `DataFrame`,
// `from pandas import DataFrame`) return annotation -- no `Annotated[...]` wrapper, no
// attached Schema, and (unlike `DataFrame[Schema]`) no subscript at all. Callers try
// `extract_schema_from_annotation` first; this is the fallback for the shape that
// leaves with no schema name to extract, which is what most third-party return
// annotations look like (a `py.typed` package has no reason to know about this
// project's `Schema` classes). The caller registers this as an unresolved schema
// (empty column list, `unresolved_schemas`) -- recognized as a DataFrame rather
// than untracked, with every column access raising `unverifiable-column`.
//
// Deliberately narrower than `extract_schema_from_annotation`: no quoted
// (forward-referenced) form, since a bare `pd.DataFrame`/`pl.DataFrame` return never
// needs forward-referencing the way a self-referencing project `Schema` class might.
pub(crate) fn extract_bare_dataframe_type(expr: &Expr) -> bool {
    match expr {
        Expr::Attribute(attr) => is_frame_type(attr.attr.as_str()),
        Expr::Name(name) => is_frame_type(name.id.as_str()),
        _ => false,
    }
}

/// A parameter annotated `pd.DataFrame`, `pl.DataFrame` or `pl.LazyFrame`: certainly a frame, with
/// columns the signature does not give.
pub(crate) fn extract_frame_parameter_type(expr: &Expr) -> bool {
    let name = match expr {
        Expr::Attribute(attr) => attr.attr.as_str(),
        Expr::Name(name) => name.id.as_str(),
        _ => return false,
    };
    is_frame_type(name) || name == "LazyFrame"
}
