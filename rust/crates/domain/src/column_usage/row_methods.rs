use super::*;

/// Uses of the frame that narrowing the columns can't affect.
pub(crate) const HARMLESS_USES: &[&str] = &["`.index`", "`.empty`", "passed to `len()`"];

/// Prefix of the description of a use that only displays the frame. Narrowing changes what
/// is shown, never whether the program runs, so it asks for review rather than blocking.
pub(crate) const OUTPUT_USE_PREFIX: &str = "output via ";

/// Attributes and methods that describe or sample the frame without choosing a column.
/// Printed or shown bare, they only display it, so narrowing changes the output and nothing
/// else; anywhere else their result may feed code that depends on the full column list.
pub(super) const DISPLAY_ONLY_NAMES: &[&str] = &[
    "shape", "columns", "dtypes", "size", "ndim", "head", "tail", "info", "describe",
];

/// Methods that return the same columns, as a new frame, when no argument names a column.
pub(super) const SAME_COLUMNS_METHODS: &[&str] = &[
    "copy",
    "clone",
    "head",
    "tail",
    "sample",
    "sort_index",
    "ffill",
    "bfill",
    "reverse",
    "limit",
    "lazy",
    "collect",
];

/// Series methods that return a boolean mask over the same rows.
pub(super) const MASK_METHODS: &[&str] = &[
    "isin",
    "isna",
    "notna",
    "isnull",
    "notnull",
    "between",
    "duplicated",
    "eq",
    "ne",
    "lt",
    "le",
    "gt",
    "ge",
];

/// `.str` methods that return a boolean mask over the same rows.
pub(super) const STR_MASK_METHODS: &[&str] = &[
    "contains",
    "startswith",
    "endswith",
    "match",
    "fullmatch",
    "isdigit",
    "isalpha",
    "isalnum",
    "isnumeric",
    "isspace",
    "islower",
    "isupper",
];

pub(super) fn is_all_rows(expr: &Expr) -> bool {
    matches!(expr, Expr::Slice(s) if s.lower.is_none() && s.upper.is_none() && s.step.is_none())
}

/// For `.rename(columns={..})` / `.drop(columns=[..])` with literal arguments: the columns the
/// call names (they must exist in the source) and the names it introduces.
pub(super) fn editing_columns_call(call: &ast::ExprCall) -> Option<(Vec<String>, Vec<String>)> {
    let Expr::Attribute(attr) = &*call.func else {
        return None;
    };
    let only = |allowed: &[&str]| {
        call.arguments.args.is_empty()
            && call.arguments.keywords.iter().all(|k| {
                k.arg
                    .as_ref()
                    .is_some_and(|a| allowed.contains(&a.as_str()))
            })
    };
    match attr.attr.as_str() {
        "rename" if only(&["columns", "errors"]) => {
            let columns = crate::frame_ops::keyword(call, "columns")?;
            let Expr::Dict(dict) = &columns.value else {
                return None;
            };
            let mapping = crate::ast_extract::extract_string_dict(dict)?;
            let mut pairs: Vec<_> = mapping.into_iter().collect();
            pairs.sort();
            Some(pairs.into_iter().unzip())
        }
        "drop" if only(&["columns", "errors"]) => {
            let columns = crate::frame_ops::keyword(call, "columns")?;
            let dropped = crate::ast_extract::extract_string_list_or_single(&columns.value)?;
            Some((dropped, Vec::new()))
        }
        _ => None,
    }
}

/// For a polars `.filter(...)` whose predicates are built only from `pl.col("name")`,
/// `pl.lit(..)` and literals: the columns it reads. `None` for anything else, including
/// pandas' column-selecting `filter(items=..)`.
pub(super) fn polars_filter_reads(call: &ast::ExprCall) -> Option<Vec<String>> {
    let Expr::Attribute(attr) = &*call.func else {
        return None;
    };
    if attr.attr.as_str() != "filter"
        || call.arguments.args.is_empty()
        || !call.arguments.keywords.is_empty()
    {
        return None;
    }
    if !call.arguments.args.iter().all(plain) {
        return None;
    }
    let reads: Vec<String> = call
        .arguments
        .args
        .iter()
        .flat_map(crate::ast_extract::collect_pl_col_names)
        .collect();
    (!reads.is_empty()).then_some(reads)
}

pub(super) fn plain(expr: &Expr) -> bool {
    match expr {
        Expr::NumberLiteral(_)
        | Expr::BooleanLiteral(_)
        | Expr::NoneLiteral(_)
        | Expr::StringLiteral(_) => true,
        Expr::UnaryOp(u) => plain(&u.operand),
        Expr::BinOp(b) => plain(&b.left) && plain(&b.right),
        Expr::BoolOp(b) => b.values.iter().all(plain),
        Expr::Compare(c) => plain(&c.left) && c.comparators.iter().all(plain),
        Expr::List(l) => l.elts.iter().all(plain),
        Expr::Tuple(t) => t.elts.iter().all(plain),
        Expr::Call(c) => {
            if let Expr::Attribute(a) = &*c.func {
                if matches!(&*a.value, Expr::Name(n) if matches!(n.id.as_str(), "pl" | "polars")) {
                    return match a.attr.as_str() {
                        "col" => {
                            c.arguments.args.len() == 1
                                && c.arguments.keywords.is_empty()
                                && crate::ast_extract::extract_string_literal(&c.arguments.args[0])
                                    .is_some_and(|n| !n.starts_with('^') && n != "*")
                        }
                        "lit" => c.arguments.args.iter().all(plain),
                        _ => false,
                    };
                }
                return plain(&a.value)
                    && c.arguments.args.iter().all(plain)
                    && c.arguments.keywords.iter().all(|k| plain(&k.value));
            }
            false
        }
        Expr::Attribute(a) => !matches!(&*a.value, Expr::Name(_)) && plain(&a.value),
        _ => false,
    }
}

/// Row operations whose result depends on every column unless a literal `subset=` names the
/// ones compared: narrowing the load would change which rows survive.
pub(super) const ROW_DEPENDENT_METHODS: &[&str] = &[
    "drop_duplicates",
    "duplicated",
    "dropna",
    "drop_nulls",
    "unique",
];

/// For a row operation that names its key columns literally (`drop_duplicates(subset=["id"])`,
/// `dropna(subset=..)`, `sort_values("a")`, polars `unique`/`drop_nulls`/`sort`): the columns
/// it reads. The other arguments must be plain flags, so no further column can hide there.
pub(super) fn row_op_reads(call: &ast::ExprCall) -> Option<Vec<String>> {
    let Expr::Attribute(attr) = &*call.func else {
        return None;
    };
    let (key_kw, positional) = match attr.attr.as_str() {
        "drop_duplicates" | "duplicated" | "unique" | "drop_nulls" => ("subset", true),
        "dropna" => ("subset", false),
        "sort_values" | "sort" => ("by", true),
        _ => return None,
    };
    let flag = |e: &Expr| match e {
        Expr::NumberLiteral(_)
        | Expr::BooleanLiteral(_)
        | Expr::NoneLiteral(_)
        | Expr::StringLiteral(_) => true,
        Expr::List(l) => l.elts.iter().all(|x| matches!(x, Expr::BooleanLiteral(_))),
        _ => false,
    };
    let mut keys = None;
    match &*call.arguments.args {
        [] => {}
        [first] if positional => {
            keys = Some(crate::ast_extract::extract_string_list_or_single(first)?);
        }
        _ => return None,
    }
    for kw in &call.arguments.keywords {
        let name = kw.arg.as_ref()?.as_str();
        if name == key_kw && keys.is_none() {
            keys = Some(crate::ast_extract::extract_string_list_or_single(
                &kw.value,
            )?);
        } else if name == "inplace" || name == "axis" || !flag(&kw.value) {
            return None;
        }
    }
    keys.filter(|k| !k.is_empty())
}

/// For an `.assign(...)`/`.with_columns(...)` call: `Some(Some(names))` when the columns it
/// adds can be read, `Some(None)` when they can't, `None` for any other call.
pub(super) fn adding_columns_call(call: &ast::ExprCall) -> Option<Option<Vec<String>>> {
    let Expr::Attribute(attr) = &*call.func else {
        return None;
    };
    match attr.attr.as_str() {
        "assign" => Some(crate::ast_extract::assign_output_names(call)),
        "with_columns" => Some(crate::ast_extract::with_columns_output_names(call)),
        _ => None,
    }
}

/// Whether a use stops the frame's columns from being narrowed.
pub(crate) fn blocks_narrowing(other: &OtherUse) -> bool {
    !HARMLESS_USES.contains(&other.description.as_str())
        && !other.description.starts_with(OUTPUT_USE_PREFIX)
}
