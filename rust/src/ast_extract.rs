//! Stateless `ruff_python_ast` query and extraction helpers.
//!
//! Everything here is a pure function of the AST nodes handed to it: schema-base
//! detection, string/list/dict literal extraction, annotation parsing, ORM column
//! discovery, and `pl.col()` name collection. None of it touches linter state.

use crate::constants::SQL_PRODUCING_METHODS;
use crate::CaseFold;
use ruff_python_ast::{self as ast, Expr, Stmt};
use std::collections::HashMap;

pub(crate) fn is_schema_base(name: &str) -> bool {
    matches!(
        name,
        "BaseSchema" | "DataFrameModel" | "DataFrame" | "BaseFrame"
    )
}

pub(crate) fn extract_string_literal(expr: &Expr) -> Option<&str> {
    if let Expr::StringLiteral(s) = expr {
        Some(s.value.to_str())
    } else {
        None
    }
}

// `__tablename__ = "orders"` (or `__tablename__: str = "orders"`) in a class's own
// body — the structural signature of a SQLAlchemy declarative model, checked
// instead of base-class name since the declarative base is normally imported from a
// project-local module. Deliberately does not walk inherited/mixin base classes for
// this — an abstract mixin that itself has no `__tablename__` isn't recognized.
pub(crate) fn class_body_has_tablename(class_def: &ast::StmtClassDef) -> bool {
    class_def.body.iter().any(|stmt| {
        let (target, value) = match stmt {
            Stmt::Assign(assign) => match assign.targets.as_slice() {
                [Expr::Name(n)] => (n.id.as_str(), Some(assign.value.as_ref())),
                _ => return false,
            },
            Stmt::AnnAssign(ann) => match ann.target.as_ref() {
                Expr::Name(n) => (n.id.as_str(), ann.value.as_deref()),
                _ => return false,
            },
            _ => return false,
        };
        target == "__tablename__" && value.and_then(extract_string_literal).is_some()
    })
}

// Class attribute names that never denote a mapped column, whatever they're
// assigned to.
pub(crate) const ORM_NON_COLUMN_ATTRS: &[&str] = &[
    "__tablename__",
    "__table_args__",
    "__mapper_args__",
    "__table__",
    "metadata",
    "registry",
    "query",
];

// Calls that construct a legitimate class attribute with no corresponding database
// column — excluded rather than swept in as a column the way the permissive
// typedframes-schema extractor's fallback (used for `is_schema_base` classes) would.
pub(crate) const ORM_NON_COLUMN_CALLS: &[&str] = &[
    "relationship",
    "column_property",
    "association_proxy",
    "declared_attr",
    "query_expression",
    "synonym",
];

// Column extractor for a SQLAlchemy declarative model (see `class_body_has_tablename`).
// Deliberately separate from the typedframes-schema extractor above rather than
// sharing it: that extractor's fallback treats *any* annotated or assigned class
// attribute as a column, which on a real model would sweep in `__tablename__`,
// `__table_args__`, `relationship(...)` attributes, and `Mapped[list[...]]`
// to-many-relationship annotations. This one uses an allowlist instead: an
// attribute is a column only if it's a `Column(...)`/`mapped_column(...)` call
// (optionally wrapped in `deferred(...)`), or a bare `x: Mapped[T]` with no value
// where `T` isn't itself a relationship shape.
pub(crate) fn extract_orm_columns(class_def: &ast::StmtClassDef) -> Vec<String> {
    let mut columns = Vec::new();
    for body_stmt in &class_def.body {
        match body_stmt {
            Stmt::AnnAssign(ann) => {
                let Expr::Name(name) = ann.target.as_ref() else {
                    continue;
                };
                let attr_name = name.id.as_str();
                if attr_name.starts_with('_') || ORM_NON_COLUMN_ATTRS.contains(&attr_name) {
                    continue;
                }
                match &ann.value {
                    Some(value) => {
                        if let Some(cols) = orm_column_from_call(value, attr_name) {
                            columns.extend(cols);
                        }
                    }
                    None => {
                        if !is_relationship_annotation(&ann.annotation) {
                            columns.push(attr_name.to_string());
                        }
                    }
                }
            }
            Stmt::Assign(assign) => {
                let [Expr::Name(name)] = assign.targets.as_slice() else {
                    continue;
                };
                let attr_name = name.id.as_str();
                if attr_name.starts_with('_') || ORM_NON_COLUMN_ATTRS.contains(&attr_name) {
                    continue;
                }
                if let Some(cols) = orm_column_from_call(&assign.value, attr_name) {
                    columns.extend(cols);
                }
            }
            _ => {}
        }
    }
    columns.sort();
    columns.dedup();
    columns
}

// `Column(...)` / `mapped_column(...)` / `deferred(Column(...))` → the resulting
// column name(s): the attribute name, plus a DB-name override from a leading
// positional string literal or a `name=` keyword when it differs from the
// attribute name — SQLAlchemy allows code to reference either spelling
// (`mapped_column("db_name", ...)` puts the real DB name in the first positional
// arg), so registering both avoids a spurious unknown-column on whichever one a
// caller writes. `relationship(...)` and friends (`ORM_NON_COLUMN_CALLS`) return
// `None` — excluded rather than swept in, unlike the permissive fallback the
// non-ORM schema extractor above uses.
pub(crate) fn orm_column_from_call(value: &Expr, attr_name: &str) -> Option<Vec<String>> {
    let Expr::Call(call) = value else {
        return None;
    };
    let fn_name = match &*call.func {
        Expr::Name(n) => n.id.as_str(),
        Expr::Attribute(a) => a.attr.as_str(),
        _ => return None,
    };
    if fn_name == "deferred" {
        return call
            .arguments
            .args
            .first()
            .and_then(|inner| orm_column_from_call(inner, attr_name));
    }
    if ORM_NON_COLUMN_CALLS.contains(&fn_name) {
        return None;
    }
    if fn_name != "Column" && fn_name != "mapped_column" {
        return None;
    }

    let mut names = vec![attr_name.to_string()];
    if let Some(db_name) = call.arguments.args.first().and_then(extract_string_literal) {
        if db_name != attr_name {
            names.push(db_name.to_string());
        }
    }
    for keyword in &call.arguments.keywords {
        if keyword.arg.as_ref().map(|s| s.as_str()) == Some("name") {
            if let Some(db_name) = extract_string_literal(&keyword.value) {
                if db_name != attr_name && !names.iter().any(|n| n == db_name) {
                    names.push(db_name.to_string());
                }
            }
        }
    }
    Some(names)
}

// `Mapped[list[...]]` / `Mapped[List[...]]` / `Mapped[Set[...]]` (a to-many
// relationship's typing shape) or `Mapped["OtherModel"]` (a quoted forward
// reference — the idiom for a to-one relationship typed without importing the
// referenced class) — never a real column, whatever the bare-annotation fallback
// in `extract_orm_columns` would otherwise assume.
pub(crate) fn is_relationship_annotation(annotation: &Expr) -> bool {
    let Expr::Subscript(sub) = annotation else {
        return false;
    };
    let is_mapped = match &*sub.value {
        Expr::Name(n) => n.id.as_str() == "Mapped",
        Expr::Attribute(a) => a.attr.as_str() == "Mapped",
        _ => false,
    };
    if !is_mapped {
        return false;
    }
    match &*sub.slice {
        Expr::StringLiteral(_) => true,
        Expr::Subscript(inner) => {
            let collection_name = match &*inner.value {
                Expr::Name(n) => Some(n.id.as_str()),
                Expr::Attribute(a) => Some(a.attr.as_str()),
                _ => None,
            };
            matches!(
                collection_name,
                Some("list" | "List" | "set" | "Set" | "Sequence")
            )
        }
        _ => false,
    }
}

// Check if a type name is a DataFrame/Frame type
pub(crate) fn is_frame_type(name: &str) -> bool {
    name == "DataFrame"
}

// Extract schema name from a type annotation like DataFrame[Schema] or Annotated[pd.DataFrame, Schema]
pub(crate) fn extract_schema_from_annotation(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Subscript(subscript) => {
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
                    if let Expr::Tuple(tuple) = &*subscript.slice {
                        if tuple.elts.len() >= 2 {
                            if let Expr::Name(schema_name) = &tuple.elts[1] {
                                return Some(schema_name.id.as_str());
                            }
                        }
                    }
                }
            }
            None
        }
        Expr::StringLiteral(s) => {
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
        _ => None,
    }
}

// Extract a list of string literals from a `["a", "b", ...]` list expression.
// Returns None if the expression is not a list or any element is not a string literal.
pub(crate) fn extract_string_list(expr: &Expr) -> Option<Vec<String>> {
    if let Expr::List(list) = expr {
        let mut result = Vec::new();
        for el in &list.elts {
            if let Expr::StringLiteral(s) = el {
                result.push(s.value.to_str().to_string());
            } else {
                return None;
            }
        }
        Some(result)
    } else {
        None
    }
}

// Extract columns from a list or single string expression.
pub(crate) fn extract_string_list_or_single(expr: &Expr) -> Option<Vec<String>> {
    match expr {
        Expr::List(_) => extract_string_list(expr),
        Expr::StringLiteral(s) => Some(vec![s.value.to_str().to_string()]),
        _ => None,
    }
}

// Extract column names from a load function call: `usecols`/`columns` kwarg,
// `dtype`/`schema` dict keys, or — for SQL-shaped load functions — the SELECT list
// of a literal SQL string. Returns the columns alongside which family satisfied the
// extraction, so the caller can phrase a kind-appropriate `untracked-dataframe` hint
// when extraction fails.

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

// Detects `df = <recv>.get_historical_features(..., features=<param>, ...).to_df()`
// (the chained form only — see ParamGovernedTemplate's doc comment) as a direct
// top-level statement in `func_def`'s own body, where `<param>` is a bare name
// matching one of the function's own parameters, followed later in the same body by
// at least one `<target>["col"]` access. Returns `None` if no such shape is found,
// or if the shape is found but nothing subscripts its result (nothing to check).

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

// Register `target_names` as a DataFrame with an ordinary (exact-match) schema
// inferred from a literal SQL string, or an untracked-dataframe warning if `sql`
// couldn't be resolved to one. Shared by the chained-finalize
// (`client.query(sql).to_dataframe()`) and cursor (`cursor.fetch_pandas_all()`)
// connector patterns, which differ only in how they locate the SQL text.

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

// Safety-checked read of a `.sql` file traced back from a load call. The real
// security boundary is project-root containment, checked below AFTER
// canonicalizing both sides (which also catches symlink escapes, since
// canonicalization follows symlinks) — NOT rejecting absolute paths outright, since
// `resolve_path_arg`'s `Path(__file__).parent / "x.sql"` case legitimately produces
// an absolute path whenever the file being checked was itself passed in as an
// absolute path (the normal case for every real caller). An absolute path a user
// wrote directly, e.g. `Path("/etc/passwd")`, still gets rejected: it canonicalizes
// to itself, which isn't under the project root. Also refuses: any extension other
// than `.sql`; a budget of more than `MAX_SQL_FILE_READS_PER_FILE` reads per file
// checked; files over `MAX_SQL_FILE_BYTES`; and anything that isn't a plain file (a
// FIFO under the project root would otherwise hang the linter, since reading one
// blocks until a writer opens it).

pub(crate) fn extract_drop_columns(call: &ast::ExprCall) -> Option<Vec<String>> {
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
        if let Expr::NumberLiteral(n) = &axis_kw.value {
            if let ast::Number::Int(ref i) = n.value {
                if i.as_u64() == Some(1) {
                    if let Some(first_arg) = call.arguments.args.first() {
                        return extract_string_list_or_single(first_arg);
                    }
                }
            }
        }
        return None; // axis present but not 1 → row drop
    }

    // No axis kwarg → polars pattern, use first positional arg
    if let Some(first_arg) = call.arguments.args.first() {
        return extract_string_list_or_single(first_arg);
    }

    None
}

// Extract rename mapping from a rename() call: {"old": "new", ...}.
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

// Create a synthetic inferred schema and register it. Returns the schema name.

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
pub(crate) fn collect_pl_col_names(expr: &Expr) -> Vec<String> {
    if let Some(name) = extract_pl_col_name(expr) {
        return vec![name];
    }
    match expr {
        Expr::Call(call) => {
            let mut names = Vec::new();
            if let Expr::Attribute(attr) = &*call.func {
                names.extend(collect_pl_col_names(&attr.value));
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
        _ => Vec::new(),
    }
}

// Does `expr` produce a value derived from a tainted variable — i.e. should its
// assignment target also be considered tainted?  Two forms: a plain alias
// (`x = df`) and a delegate call forwarding a tainted variable as the first
// positional argument (`x = preproc(df)`, `x = infer(step1)`, …). This is what
// lets taint follow a `step1 = preproc(df); step2 = infer(step1)` chain.
//
// Deliberately does NOT treat `x = df[["a", "b"]]` (a literal list/string slice)
// as tainting `x`: that expression already produces a *fully known* schema for
// `x` via the existing multi-column-subscript tracking in the Assign handler
// above (make_inferred_schema), independent of this heuristic. A later
// `x["bad_col"]` is therefore an outright, certain unknown-column bug local to
// this function — caught directly by visit_expr against that inferred schema —
// not an ambiguous "maybe missing from the caller" requirement to add to `df`'s
// contract. Folding it in here would double-report the same bug two ways.

#[cfg(test)]
mod tests {
    use super::*;
    use ruff_python_parser::parse_module;

    #[test]
    fn test_should_detect_base_schema_class() {
        // arrange/act/assert
        assert!(is_schema_base("BaseSchema"));
        assert!(is_schema_base("DataFrameModel"));
        assert!(is_schema_base("DataFrame"));
        assert!(is_schema_base("BaseFrame"));
        assert!(!is_schema_base("SomeOtherClass"));
    }

    #[test]
    fn test_extract_schema_from_annotation() {
        let source = "x: DataFrame[MySchema] = df";
        let parsed = parse_module(source).unwrap();
        let stmt = &parsed.into_syntax().body[0];
        if let Stmt::AnnAssign(ann) = stmt {
            let schema = extract_schema_from_annotation(&ann.annotation);
            assert_eq!(schema, Some("MySchema"));
        } else {
            panic!("Expected AnnAssign");
        }
    }
}
