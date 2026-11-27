use super::*;

/// The column names a `.assign(...)` call adds, from its keywords.
pub(crate) fn assign_output_names(call: &ast::ExprCall) -> Option<Vec<String>> {
    let mut names = Vec::new();
    for keyword in &call.arguments.keywords {
        match &keyword.arg {
            Some(name) => names.push(name.to_string()),
            // `**{"a": ..}` names its columns; `**mapping` can name any.
            None => {
                let Expr::Dict(dict) = &keyword.value else {
                    return None;
                };
                for item in &dict.items {
                    names.push(extract_string_literal(item.key.as_ref()?)?.to_string());
                }
            }
        }
    }
    Some(names)
}

/// The name a polars expression outputs: its `.alias("name")`, else the column its chain
/// starts from (`pl.col("a").sum()` is `a`). `None` when the name can't be read
/// statically -- an expression over several columns, a renaming namespace, `pl.all()`.
pub(crate) fn pl_expr_output_name(expr: &Expr) -> Option<String> {
    if let Some(name) = extract_pl_col_name(expr) {
        return Some(name);
    }
    let Expr::Call(call) = expr else {
        return None;
    };
    let Expr::Attribute(attr) = &*call.func else {
        return None;
    };
    if attr.attr.as_str() == "alias" {
        return Some(extract_string_literal(call.arguments.args.first()?)?.to_string());
    }
    let mut receiver = &*attr.value;
    loop {
        if let Some(name) = extract_pl_col_name(receiver) {
            return Some(name);
        }
        match receiver {
            Expr::Call(inner) => match &*inner.func {
                Expr::Attribute(a) => {
                    if a.attr.as_str() == "alias" {
                        return None;
                    }
                    receiver = &*a.value;
                }
                _ => return None,
            },
            // A namespace (`.str`, `.dt`) keeps the name; these rename.
            Expr::Attribute(a) if matches!(a.attr.as_str(), "name" | "struct") => return None,
            Expr::Attribute(a) => receiver = &*a.value,
            _ => return None,
        }
    }
}

/// The column names a polars `.with_columns(...)`/`.agg(...)` call outputs, or `None` when
/// an expression's output name can't be read statically.
pub(crate) fn with_columns_output_names(call: &ast::ExprCall) -> Option<Vec<String>> {
    let mut names = assign_output_names(call)?;
    for arg in &call.arguments.args {
        names.push(pl_expr_output_name(arg)?);
    }
    Some(names)
}

/// Output names of `.with_columns(...)` positional expressions named by the column their
/// chain starts from rather than an `.alias(...)`: `pl.col("x").cast(..)` outputs `x`.
pub(crate) fn with_columns_unaliased_outputs(call: &ast::ExprCall) -> Vec<String> {
    call.arguments
        .args
        .iter()
        .filter(|arg| {
            !matches!(arg, Expr::Call(c) if matches!(&*c.func, Expr::Attribute(a) if a.attr.as_str() == "alias"))
        })
        .filter_map(pl_expr_output_name)
        .collect()
}

/// The key columns of a polars `.group_by(...)`/`.join(on=...)` argument: a string, a
/// `pl.col("x")`, or a list of them.
pub(crate) fn literal_key_columns(expr: &Expr) -> Option<Vec<String>> {
    match expr {
        Expr::StringLiteral(s) => Some(vec![s.value.to_str().to_string()]),
        Expr::List(list) => list.elts.iter().map(single_key_column).collect(),
        other => single_key_column(other).map(|c| vec![c]),
    }
}

fn single_key_column(expr: &Expr) -> Option<String> {
    extract_string_literal(expr)
        .map(str::to_string)
        .or_else(|| extract_pl_col_name(expr))
}

/// The key columns given to `group_by`: every positional argument, or `by=`.
pub(crate) fn group_by_keys(call: &ast::ExprCall) -> Option<Vec<String>> {
    let mut keys = Vec::new();
    for arg in &call.arguments.args {
        keys.extend(literal_key_columns(arg)?);
    }
    if let Some(by) = call
        .arguments
        .keywords
        .iter()
        .find(|k| k.arg.as_ref().map(|a| a.as_str()) == Some("by"))
    {
        keys.extend(literal_key_columns(&by.value)?);
    }
    (!keys.is_empty()).then_some(keys)
}

pub(crate) fn extract_pl_col_name(expr: &Expr) -> Option<String> {
    if let Expr::Call(call) = expr {
        let is_col_call = match &*call.func {
            Expr::Attribute(attr) => {
                attr.attr.as_str() == "col"
                    && matches!(&*attr.value, Expr::Name(n) if matches!(n.id.as_str(), "pl" | "polars"))
            }
            Expr::Name(n) => n.id.as_str() == "col",
            _ => false,
        };
        if is_col_call {
            return call
                .arguments
                .args
                .first()
                .and_then(|a| extract_string_literal(a))
                .map(|s| s.to_string());
        }
    }
    None
}

// Recursively collect all column names referenced via `pl.col("name")` / `col("name")`
// in an expression tree. Handles chained calls, lists, tuples, comparisons, and binary ops.
// Column names for `df.select(...)`: either a single list-literal argument
// (`select(["a", "b"])`, the existing pandas-`.filter`-adjacent shape), or one or
// more positional arguments each resolving to a single column name -- a bare string
// literal or a `pl.col("name")` call (`select("a", "b")`,
// `select(pl.col("a"), pl.col("b"))`, or a mix of the two). `None` when any argument
// isn't one of those recognized shapes (`pl.all()`, `pl.exclude(...)`, a dynamic
// expression, ...) -- the caller treats that the same as an unrecognized edit, not as
// "no columns selected".
// Whether `df.filter(...)` is pandas' column/index-label-selecting form
// (`items=`/`like=`/`regex=`) rather than the row-filtering positional-mask/callable
// form (`df.filter(mask)`) both pandas and polars also spell this same method name
// for -- the two do fundamentally different things and must not share handling.
pub(crate) fn filter_is_column_selecting(call: &ast::ExprCall) -> bool {
    let has_selector_kwarg = call.arguments.keywords.iter().any(|kw| {
        matches!(
            kw.arg.as_ref().map(|a| a.as_str()),
            Some("items") | Some("like") | Some("regex")
        )
    });
    if !has_selector_kwarg {
        return false;
    }
    // `.filter`'s default axis is columns; `axis=0`/`axis="index"` redirects
    // items=/like=/regex= to INDEX LABELS instead, which this checker has no model
    // for -- treat that the same as the row-filtering `.filter(mask)` form (leave it
    // alone entirely) rather than validate it against the column schema.
    let axis_kwarg = call
        .arguments
        .keywords
        .iter()
        .find(|kw| kw.arg.as_ref().map(|a| a.as_str()) == Some("axis"));
    match axis_kwarg {
        Some(axis_kw) => !axis_kwarg_selects_index(&axis_kw.value),
        None => true,
    }
}

fn axis_kwarg_selects_index(value: &Expr) -> bool {
    match value {
        Expr::NumberLiteral(n) => matches!(&n.value, ast::Number::Int(i) if i.as_u64() == Some(0)),
        Expr::StringLiteral(s) => s.value.to_str() == "index",
        _ => false,
    }
}

// The literal column list from `df.filter(items=[...])`. `None` for `like=`/`regex=`
// (pattern-based, inherently dynamic) or a non-literal `items=` -- still a column
// selection, just not one whose exact result can be known statically.
pub(crate) fn extract_filter_items_columns(call: &ast::ExprCall) -> Option<Vec<String>> {
    let items = call
        .arguments
        .keywords
        .iter()
        .find(|kw| kw.arg.as_ref().map(|a| a.as_str()) == Some("items"))?;
    extract_string_list(&items.value)
}

pub(crate) fn extract_select_args_columns(call: &ast::ExprCall) -> Option<Vec<String>> {
    if call.arguments.args.len() == 1 {
        if let Expr::List(list) = &call.arguments.args[0] {
            let mut cols = Vec::new();
            for el in &list.elts {
                cols.push(extract_string_literal(el)?.to_string());
            }
            return Some(cols);
        }
    }
    if call.arguments.args.is_empty() {
        return None;
    }
    call.arguments
        .args
        .iter()
        .map(|arg| {
            extract_string_literal(arg)
                .map(str::to_string)
                .or_else(|| extract_pl_col_name(arg))
        })
        .collect()
}

pub(crate) fn collect_pl_col_names(expr: &Expr) -> Vec<String> {
    if let Some(name) = extract_pl_col_name(expr) {
        return vec![name];
    }
    match expr {
        Expr::Call(call) => {
            let mut names = Vec::new();
            if let Expr::Attribute(attr) = &*call.func {
                names.extend(collect_pl_col_names(&attr.value));
                // `.over("a", ["b"])`, `.over(partition_by=..)`: bare strings are column names.
                if attr.attr.as_str() == "over" {
                    for arg in call.arguments.args.iter().chain(
                        call.arguments
                            .keywords
                            .iter()
                            .filter(|k| k.arg.as_ref().map(|a| a.as_str()) == Some("partition_by"))
                            .map(|k| &k.value),
                    ) {
                        names.extend(extract_string_list_or_single(arg).unwrap_or_default());
                    }
                }
            }
            for arg in &call.arguments.args {
                names.extend(collect_pl_col_names(arg));
            }
            for kw in &call.arguments.keywords {
                names.extend(collect_pl_col_names(&kw.value));
            }
            names
        }
        Expr::List(list) => list.elts.iter().flat_map(collect_pl_col_names).collect(),
        Expr::Tuple(tuple) => tuple.elts.iter().flat_map(collect_pl_col_names).collect(),
        Expr::Compare(compare) => {
            let mut names = collect_pl_col_names(&compare.left);
            for comp in compare.comparators.iter() {
                names.extend(collect_pl_col_names(comp));
            }
            names
        }
        Expr::BinOp(binop) => {
            let mut names = collect_pl_col_names(&binop.left);
            names.extend(collect_pl_col_names(&binop.right));
            names
        }
        Expr::BoolOp(boolop) => boolop
            .values
            .iter()
            .flat_map(collect_pl_col_names)
            .collect(),
        Expr::UnaryOp(unary) => collect_pl_col_names(&unary.operand),
        // A namespace step: `pl.col("a").list.len()`.
        Expr::Attribute(attr) => collect_pl_col_names(&attr.value),
        _ => Vec::new(),
    }
}
