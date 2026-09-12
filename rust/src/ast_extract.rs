//! Stateless `ruff_python_ast` query and extraction helpers.
//!
//! Everything here is a pure function of the AST nodes handed to it: schema-base
//! detection, string/list/dict literal extraction, annotation parsing, ORM column
//! discovery, and `pl.col()` name collection. None of it touches linter state.

use crate::constants::{
    SPARK_FUNCTIONS_MODULES, SPARK_READER_CHAIN_METHODS, SQL_PRODUCING_METHODS,
};
use crate::linter::CaseFold;
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

// Recognize a BARE `pd.DataFrame` / `pl.DataFrame` (or a bare-imported `DataFrame`,
// `from pandas import DataFrame`) return annotation -- no `Annotated[...]` wrapper, no
// attached Schema, and (unlike `DataFrame[Schema]`) no subscript at all. Callers try
// `extract_schema_from_annotation` first; this is the fallback for the shape that
// leaves with no schema name to extract, which is what most third-party return
// annotations look like (a `py.typed` package has no reason to know about this
// project's `Schema` classes). The caller registers a match with an *open* schema
// (empty column list, `open_schemas`) the same way `register_feast_dataframe` already
// does -- "we know it's a DataFrame, we don't know its columns" is strictly better
// than leaving the call untracked, and never manufactures a false unknown-column.
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

    // No axis kwarg → the varargs convention shared by polars
    // (`df.drop("a", "b")`, signature `drop(*columns, strict=True)`) and PySpark
    // (`df.drop("a", "b")`, signature `drop(*cols)`, which also accepts Column
    // objects such as `F.col("a")` or `df.a`).
    //
    // Every positional argument has to resolve for the varargs reading to be used,
    // and the single-argument fallback below is kept verbatim for the case where one
    // doesn't: pandas' deprecated positional-axis form `df.drop("a", 1)` must keep
    // behaving exactly as it did (drop column `a`) rather than degrade to "can't
    // tell" now that a second positional argument is inspected at all.
    let mut all_args: Vec<String> = Vec::new();
    let mut every_arg_resolved = !call.arguments.args.is_empty();
    for arg in call.arguments.args.iter() {
        match extract_string_list_or_single(arg)
            .or_else(|| extract_col_ref_name(arg).map(|c| vec![c]))
        {
            Some(cols) => all_args.extend(cols),
            None => {
                every_arg_resolved = false;
                break;
            }
        }
    }
    if every_arg_resolved {
        return Some(all_args);
    }

    if let Some(first_arg) = call.arguments.args.first() {
        return extract_string_list_or_single(first_arg);
    }

    None
}

// Column names out of the three shapes PySpark accepts wherever a schema is
// declared — `spark.read.schema(<here>)`, a `schema=` keyword on a reader method,
// or `spark.createDataFrame(data, <here>)`:
//
//   * `StructType([StructField("id", IntegerType()), ...])` — the field names
//   * `["id", "name"]` — a bare list/tuple of column names
//   * `"id INT, name STRING"` — a DDL string (see `parse_spark_ddl_schema`)
//
// Returns `None` for anything else (a name referring to a schema built elsewhere, a
// `StructType().add(...)` builder chain, an f-string, …), which the caller reports
// as `untracked-dataframe` rather than guessing at.
pub(crate) fn extract_spark_schema_columns(expr: &Expr) -> Option<Vec<String>> {
    match expr {
        Expr::StringLiteral(s) => parse_spark_ddl_schema(s.value.to_str()),
        Expr::List(list) => collect_string_elements(&list.elts),
        Expr::Tuple(tuple) => collect_string_elements(&tuple.elts),
        Expr::Call(call) => {
            let is_struct_type = match &*call.func {
                Expr::Name(n) => n.id.as_str() == "StructType",
                Expr::Attribute(a) => a.attr.as_str() == "StructType",
                _ => false,
            };
            if !is_struct_type {
                return None;
            }
            // `StructType([...])` — the field list is the only positional argument.
            // A bare `StructType()` builder (fields added later via `.add()`) has no
            // resolvable column set and correctly falls through to `None`.
            let fields = call.arguments.args.first()?;
            let elts = match fields {
                Expr::List(list) => &list.elts,
                Expr::Tuple(tuple) => &tuple.elts,
                _ => return None,
            };
            let mut names = Vec::with_capacity(elts.len());
            for elt in elts {
                names.push(extract_struct_field_name(elt)?);
            }
            Some(names)
        }
        _ => None,
    }
}

fn collect_string_elements(elts: &[Expr]) -> Option<Vec<String>> {
    let mut names = Vec::with_capacity(elts.len());
    for elt in elts {
        names.push(extract_string_literal(elt)?.to_string());
    }
    Some(names)
}

// The field name out of a `StructField("id", IntegerType(), True)` call — its first
// positional argument, or an explicit `name=` keyword.
fn extract_struct_field_name(expr: &Expr) -> Option<String> {
    let Expr::Call(call) = expr else {
        return None;
    };
    let is_struct_field = match &*call.func {
        Expr::Name(n) => n.id.as_str() == "StructField",
        Expr::Attribute(a) => a.attr.as_str() == "StructField",
        _ => false,
    };
    if !is_struct_field {
        return None;
    }
    for keyword in &call.arguments.keywords {
        if keyword.arg.as_ref().map(|s| s.as_str()) == Some("name") {
            return extract_string_literal(&keyword.value).map(|s| s.to_string());
        }
    }
    call.arguments
        .args
        .first()
        .and_then(|a| extract_string_literal(a))
        .map(|s| s.to_string())
}

// Column names out of a Spark DDL schema string: `"id INT, name STRING"`.
//
// Splitting on commas has to respect nesting, because a complex type carries its own
// commas — `"id INT, tags ARRAY<STRING>, meta STRUCT<a: INT, b: INT>"` is three
// fields, not four. Depth is tracked across `<>`, `()` and `[]`; anything that
// leaves the string unbalanced, or a field whose first token isn't a plain
// identifier (a backtick-quoted name, an empty segment, a bare type with no name),
// makes the whole parse fail rather than produce a half-right column list.
pub(crate) fn parse_spark_ddl_schema(ddl: &str) -> Option<Vec<String>> {
    let mut names = Vec::new();
    let mut depth: i32 = 0;
    let mut field = String::new();
    for ch in ddl.chars() {
        match ch {
            '<' | '(' | '[' => {
                depth += 1;
                field.push(ch);
            }
            '>' | ')' | ']' => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
                field.push(ch);
            }
            ',' if depth == 0 => {
                names.push(ddl_field_name(&field)?);
                field.clear();
            }
            _ => field.push(ch),
        }
    }
    if depth != 0 {
        return None;
    }
    names.push(ddl_field_name(&field)?);
    Some(names)
}

// The name half of one DDL field spec (`"  id INT  "` -> `"id"`). Requires a name
// followed by at least one more token (the type), and an identifier-shaped name, so
// that a colon-separated struct-field spec or a quoted identifier is rejected
// outright instead of silently mangled.
fn ddl_field_name(field: &str) -> Option<String> {
    let mut parts = field.split_whitespace();
    let name = parts.next()?;
    parts.next()?;
    let identifier_shaped = !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_alphanumeric() || c == '_');
    identifier_shaped.then(|| name.to_string())
}

// Walk a `spark.read…` chain looking for the reader's declared schema.
//
// Returns `None` if `expr` isn't a DataFrameReader chain at all (so the caller
// leaves the call alone), `Some(None)` for a reader with no `.schema(...)` in the
// chain, and `Some(Some(expr))` for the argument of the `.schema(...)` that is
// there. Chain methods that return the reader itself (`.option`, `.options`,
// `.format`) are walked through; anything else ends the walk without a match.
//
// The chain must bottom out at a literal `.read` attribute, which is what makes
// this specific to Spark without hard-coding a receiver name: `spark.read`,
// `self.spark.read` and `get_session().read` all qualify, while an unrelated
// `foo.csv(...)` does not.
pub(crate) fn spark_reader_schema(expr: &Expr) -> Option<Option<&Expr>> {
    match expr {
        Expr::Attribute(attr) if attr.attr.as_str() == "read" => Some(None),
        Expr::Call(call) => {
            let Expr::Attribute(attr) = &*call.func else {
                return None;
            };
            let inner = spark_reader_schema(&attr.value)?;
            match attr.attr.as_str() {
                "schema" => Some(call.arguments.args.first().or(inner)),
                m if SPARK_READER_CHAIN_METHODS.contains(&m) => Some(inner),
                _ => None,
            }
        }
        _ => None,
    }
}

// One resolved item of a PySpark `select(...)`: the name it contributes to the
// output schema, and — when the item names an existing column directly rather than
// deriving one — the source column that must exist on the receiver.
pub(crate) struct SparkSelectItem {
    pub(crate) output: String,
    // Only ever `Some` for a bare string item, `df.select("id")`. That is the one
    // select shape nothing else in the linter looks at, so it is the one this has to
    // report.
    //
    // Every other shape is already validated where it stands, and repeating the
    // check here would report the same mistake twice:
    //   * `F.col("id")` / `pl.col("id")` — by `validate_pl_col_args_on_receiver`,
    //     which walks the whole call's arguments;
    //   * `df.id` / `df["id"]` — by `visit_expr`, which validates attribute and
    //     subscript accesses on a tracked variable wherever they appear.
    // And for a renamed item such as `F.col("id").alias("user_id")` the output name
    // is deliberately NOT a receiver column, so checking it would be a guaranteed
    // false positive.
    pub(crate) source: Option<String>,
}

// Resolve every item of a PySpark `df.select(...)`, or `None` if any of them can't
// be resolved.
//
// Spark's `select(*cols)` takes column-name varargs, a single sequence, or `Column`
// objects, so all of these are the same selection:
//
//   df.select("id", "name")                  df.select(["id", "name"])
//   df.select(F.col("id"), df.name)          df.select(df["id"], F.col("name"))
//
// A `"*"` selector, a `Column` built from an arithmetic expression, and anything
// else unrecognized all yield `None` — the caller then falls back to carrying the
// receiver's schema forward unchanged, which can only under-report (a column the
// select dropped stays "known"), never invent a column error.
//
// `recv_name` is the receiver variable, so that `df.name` / `df["name"]` inside its
// own `select` are read as column references rather than skipped.
pub(crate) fn extract_spark_select_items(
    call: &ast::ExprCall,
    recv_name: &str,
) -> Option<Vec<SparkSelectItem>> {
    if !call.arguments.keywords.is_empty() || call.arguments.args.is_empty() {
        return None;
    }
    // The single-sequence overload: `df.select(["id", "name"])`.
    let items: Vec<&Expr> = match call.arguments.args.as_ref() {
        [Expr::List(list)] => list.elts.iter().collect(),
        [Expr::Tuple(tuple)] => tuple.elts.iter().collect(),
        args => args.iter().collect(),
    };
    let mut resolved = Vec::with_capacity(items.len());
    for item in items {
        resolved.push(spark_select_item(item, recv_name)?);
    }
    Some(resolved)
}

// An item whose output name is a column of the receiver, but which some other part
// of the linter already validates — see `SparkSelectItem::source`.
fn validated_elsewhere(name: String) -> SparkSelectItem {
    SparkSelectItem {
        output: name,
        source: None,
    }
}

fn spark_select_item(item: &Expr, recv_name: &str) -> Option<SparkSelectItem> {
    match item {
        // A literal name. `"*"` is a whole-frame selector, not a column.
        Expr::StringLiteral(s) => {
            let name = s.value.to_str();
            (name != "*").then(|| SparkSelectItem {
                source: Some(name.to_string()),
                output: name.to_string(),
            })
        }
        // `df.name` — attribute-style column access on the receiver itself.
        Expr::Attribute(attr) => {
            let Expr::Name(base) = &*attr.value else {
                return None;
            };
            (base.id.as_str() == recv_name).then(|| validated_elsewhere(attr.attr.to_string()))
        }
        // `df["name"]` — string-subscript column access on the receiver itself.
        Expr::Subscript(subscript) => {
            let Expr::Name(base) = &*subscript.value else {
                return None;
            };
            if base.id.as_str() != recv_name {
                return None;
            }
            extract_string_literal(&subscript.slice).map(|s| validated_elsewhere(s.to_string()))
        }
        Expr::Call(inner) => {
            if let Some(name) = extract_col_ref_name(item) {
                return Some(validated_elsewhere(name));
            }
            // `<anything>.alias("out")` — the alias is the output name whatever the
            // expression it renames, so the source is deliberately left unresolved.
            let Expr::Attribute(attr) = &*inner.func else {
                return None;
            };
            if attr.attr.as_str() != "alias" {
                return None;
            }
            inner
                .arguments
                .args
                .first()
                .and_then(|a| extract_string_literal(a))
                .map(|s| SparkSelectItem {
                    output: s.to_string(),
                    source: None,
                })
        }
        _ => None,
    }
}

// Is `expr` a `spark.read…csv(path)` / `.parquet(path)` / … call — the receiver
// shape that makes a directly chained `.select(...)` the authoritative column set
// for a read whose schema is otherwise unknowable? See `spark_reader_schema` for
// what qualifies as a reader chain.
pub(crate) fn is_spark_read_call(expr: &Expr) -> bool {
    let Expr::Call(call) = expr else {
        return false;
    };
    let Expr::Attribute(attr) = &*call.func else {
        return false;
    };
    crate::constants::SPARK_READ_METHODS.contains(&attr.attr.as_str())
        && spark_reader_schema(&attr.value).is_some()
}

// A `SparkSession.builder…getOrCreate()` chain — the canonical way a session is
// constructed, and the signal that binds a variable name into the linter's
// `spark_sessions` set. Matched on the terminal `getOrCreate` call rather than on
// `SparkSession` itself so that the arbitrary `.appName(...)/.config(...)/.master(...)`
// middle of the chain needs no enumeration.
pub(crate) fn is_spark_session_builder(expr: &Expr) -> bool {
    let Expr::Call(call) = expr else {
        return false;
    };
    let Expr::Attribute(attr) = &*call.func else {
        return false;
    };
    if attr.attr.as_str() != "getOrCreate" {
        return false;
    }
    chain_mentions_builder(&attr.value)
}

fn chain_mentions_builder(expr: &Expr) -> bool {
    match expr {
        Expr::Attribute(attr) => {
            attr.attr.as_str() == "builder" || chain_mentions_builder(&attr.value)
        }
        Expr::Call(call) => chain_mentions_builder(&call.func),
        _ => false,
    }
}

// A `SparkSession` type annotation, in any of the forms an import can produce:
// `SparkSession`, `pyspark.sql.SparkSession`, or either of those quoted.
pub(crate) fn is_spark_session_annotation(expr: &Expr) -> bool {
    match expr {
        Expr::Name(n) => n.id.as_str() == "SparkSession",
        Expr::Attribute(a) => a.attr.as_str() == "SparkSession",
        Expr::StringLiteral(s) => s.value.to_str().trim().ends_with("SparkSession"),
        _ => false,
    }
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

// Just the keys of a string-keyed dict literal, for calls where the values are
// expressions rather than names — PySpark's `withColumns({"total": F.col("a") + 1})`.
pub(crate) fn extract_dict_string_keys(dict: &ast::ExprDict) -> Option<Vec<String>> {
    let mut keys = Vec::with_capacity(dict.items.len());
    for item in &dict.items {
        let key = item.key.as_ref()?;
        keys.push(extract_string_literal(key)?.to_string());
    }
    Some(keys)
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

// PySpark's counterpart to `extract_pl_col_name`: a column name out of
// `F.col("x")` / `sf.col("x")` / `functions.col("x")`, the conventional aliases for
// `pyspark.sql.functions` (see `SPARK_FUNCTIONS_MODULES`).
//
// Only the *qualified* form lives here. The bare-imported `col("x")`
// (`from pyspark.sql.functions import col`) is already matched by
// `extract_pl_col_name`'s `Expr::Name` branch, which keys on the function name
// alone and so is backend-agnostic — duplicating it here would just make the two
// helpers both claim the same expression.
pub(crate) fn extract_spark_col_name(expr: &Expr) -> Option<String> {
    let Expr::Call(call) = expr else {
        return None;
    };
    let Expr::Attribute(attr) = &*call.func else {
        return None;
    };
    if attr.attr.as_str() != "col" {
        return None;
    }
    let Expr::Name(module) = &*attr.value else {
        return None;
    };
    if !SPARK_FUNCTIONS_MODULES.contains(&module.id.as_str()) {
        return None;
    }
    call.arguments
        .args
        .first()
        .and_then(|a| extract_string_literal(a))
        .map(|s| s.to_string())
}

// A column reference in either backend's idiom: polars' `pl.col("x")`/`col("x")` or
// PySpark's `F.col("x")`. Both are the same shape (a `col` call whose single
// positional argument is the column name) and both are validated against the
// receiver's schema the same way, so every caller wants both.
pub(crate) fn extract_col_ref_name(expr: &Expr) -> Option<String> {
    extract_pl_col_name(expr).or_else(|| extract_spark_col_name(expr))
}

// Recursively collect all column names referenced via `pl.col("name")` / `col("name")`
// / `F.col("name")` in an expression tree. Handles chained calls, lists, tuples,
// comparisons, and binary ops.
pub(crate) fn collect_pl_col_names(expr: &Expr) -> Vec<String> {
    if let Some(name) = extract_col_ref_name(expr) {
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

    fn expr_of(source: &str) -> ruff_python_ast::Expr {
        let parsed = parse_module(source).unwrap();
        let Stmt::Expr(stmt) = &parsed.into_syntax().body[0] else {
            panic!("expected an expression statement");
        };
        (*stmt.value).clone()
    }

    // The parsed call plus a leaked box, so tests can hand out a `&ExprCall` with a
    // lifetime long enough for the assertion that follows. Test-only.
    fn call_of(source: &str) -> &'static ast::ExprCall {
        let Expr::Call(call) = expr_of(source) else {
            panic!("expected a call expression");
        };
        Box::leak(Box::new(call))
    }

    fn select_items_of(source: &str) -> Vec<SparkSelectItem> {
        extract_spark_select_items(call_of(source), "df").expect("select should resolve")
    }

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

    fn annotation_of(source: &str) -> ruff_python_ast::Expr {
        let parsed = parse_module(source).unwrap();
        let Stmt::FunctionDef(func) = &parsed.into_syntax().body[0] else {
            panic!("Expected FunctionDef");
        };
        (*func.returns.clone().expect("expected a return annotation")).clone()
    }

    #[test]
    fn test_should_detect_bare_pandas_and_polars_dataframe_return_types() {
        assert!(extract_bare_dataframe_type(&annotation_of(
            "def f() -> pd.DataFrame: ..."
        )));
        assert!(extract_bare_dataframe_type(&annotation_of(
            "def f() -> pl.DataFrame: ..."
        )));
        // Bare-imported form: `from pandas import DataFrame`, `-> DataFrame`.
        assert!(extract_bare_dataframe_type(&annotation_of(
            "def f() -> DataFrame: ..."
        )));
    }

    #[test]
    fn test_should_not_detect_a_schema_subscripted_dataframe_as_bare() {
        // DataFrame[Schema] / Annotated[pd.DataFrame, Schema] have a real schema to
        // extract via extract_schema_from_annotation -- callers try that FIRST, so
        // extract_bare_dataframe_type only needs to reject them, not handle them.
        assert!(!extract_bare_dataframe_type(&annotation_of(
            "def f() -> DataFrame[MySchema]: ..."
        )));
        assert!(!extract_bare_dataframe_type(&annotation_of(
            "def f() -> Annotated[pd.DataFrame, MySchema]: ..."
        )));
    }

    #[test]
    fn test_should_not_detect_an_unrelated_return_type_as_a_bare_dataframe() {
        assert!(!extract_bare_dataframe_type(&annotation_of(
            "def f() -> int: ..."
        )));
        assert!(!extract_bare_dataframe_type(&annotation_of(
            "def f() -> pd.Series: ..."
        )));
        assert!(!extract_bare_dataframe_type(&annotation_of(
            "def f() -> None: ..."
        )));
    }

    #[test]
    fn test_should_extract_spark_qualified_col_names() {
        // arrange/act/assert
        for source in [
            "F.col(\"amount\")",
            "sf.col(\"amount\")",
            "functions.col(\"amount\")",
        ] {
            assert_eq!(
                extract_spark_col_name(&expr_of(source)),
                Some("amount".to_string()),
                "source: {source}"
            );
        }
    }

    #[test]
    fn test_should_not_extract_spark_col_from_an_unrelated_module_or_function() {
        // `pl.col` belongs to extract_pl_col_name; an arbitrary module alias and a
        // non-`col` spark function must both be rejected outright.
        assert_eq!(extract_spark_col_name(&expr_of("pl.col(\"a\")")), None);
        assert_eq!(extract_spark_col_name(&expr_of("mymod.col(\"a\")")), None);
        assert_eq!(extract_spark_col_name(&expr_of("F.lit(\"a\")")), None);
        assert_eq!(extract_spark_col_name(&expr_of("F.col(name)")), None);
    }

    #[test]
    fn test_should_collect_both_polars_and_spark_col_references() {
        // collect_pl_col_names is the shared entry point both backends' validation
        // goes through, so it has to see F.col alongside pl.col and bare col.
        assert_eq!(
            collect_pl_col_names(&expr_of("F.col(\"a\") + pl.col(\"b\") + col(\"c\")")),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn test_should_parse_spark_ddl_schema_strings() {
        assert_eq!(
            parse_spark_ddl_schema("id INT, name STRING"),
            Some(vec!["id".to_string(), "name".to_string()])
        );
        // Commas nested inside a complex type must not split a field.
        assert_eq!(
            parse_spark_ddl_schema("id INT, tags ARRAY<STRING>, meta STRUCT<a: INT, b: INT>"),
            Some(vec![
                "id".to_string(),
                "tags".to_string(),
                "meta".to_string()
            ])
        );
        assert_eq!(
            parse_spark_ddl_schema("amount DECIMAL(10, 2), label STRING"),
            Some(vec!["amount".to_string(), "label".to_string()])
        );
    }

    #[test]
    fn test_should_reject_ddl_schema_strings_it_cannot_read_exactly() {
        // A name with no type, an unbalanced bracket, a quoted identifier, and an
        // empty segment all fail the whole parse rather than yielding a partial
        // column list that would then manufacture unknown-column errors.
        assert_eq!(parse_spark_ddl_schema("id"), None);
        assert_eq!(parse_spark_ddl_schema("id INT, tags ARRAY<STRING"), None);
        assert_eq!(parse_spark_ddl_schema("`odd name` INT"), None);
        assert_eq!(parse_spark_ddl_schema("id INT,, name STRING"), None);
        assert_eq!(parse_spark_ddl_schema(""), None);
    }

    #[test]
    fn test_should_extract_spark_schema_columns_from_every_accepted_form() {
        assert_eq!(
            extract_spark_schema_columns(&expr_of("\"id INT, name STRING\"")),
            Some(vec!["id".to_string(), "name".to_string()])
        );
        assert_eq!(
            extract_spark_schema_columns(&expr_of("[\"id\", \"name\"]")),
            Some(vec!["id".to_string(), "name".to_string()])
        );
        assert_eq!(
            extract_spark_schema_columns(&expr_of(
                "StructType([StructField(\"id\", IntegerType()), StructField(name=\"n\", dataType=StringType())])"
            )),
            Some(vec!["id".to_string(), "n".to_string()])
        );
        assert_eq!(
            extract_spark_schema_columns(&expr_of(
                "T.StructType((StructField(\"id\", T.IntegerType()),))"
            )),
            Some(vec!["id".to_string()])
        );
    }

    #[test]
    fn test_should_not_extract_spark_schema_columns_from_unresolvable_forms() {
        // A builder chain, a schema held in a variable, and a non-literal field name
        // are all genuinely unknown at lint time.
        assert_eq!(extract_spark_schema_columns(&expr_of("StructType()")), None);
        assert_eq!(
            extract_spark_schema_columns(&expr_of("StructType().add(\"id\", \"int\")")),
            None
        );
        assert_eq!(extract_spark_schema_columns(&expr_of("my_schema")), None);
        assert_eq!(
            extract_spark_schema_columns(&expr_of(
                "StructType([StructField(name_var, IntegerType())])"
            )),
            None
        );
        assert_eq!(
            extract_spark_schema_columns(&expr_of("[\"id\", other]")),
            None
        );
    }

    #[test]
    fn test_should_walk_a_spark_reader_chain_to_its_schema() {
        // No .schema(...) anywhere in the chain -- a reader, but an undeclared one.
        assert!(matches!(
            spark_reader_schema(&expr_of("spark.read")),
            Some(None)
        ));
        assert!(matches!(
            spark_reader_schema(&expr_of("spark.read.option(\"header\", True)")),
            Some(None)
        ));
        // .schema(...) found through the reader-returning chain methods.
        assert!(matches!(
            spark_reader_schema(&expr_of(
                "spark.read.format(\"csv\").option(\"header\", True).schema(\"id INT\").option(\"sep\", \",\")"
            )),
            Some(Some(_))
        ));
        // Any receiver works, as long as the chain bottoms out at `.read`.
        assert!(matches!(
            spark_reader_schema(&expr_of("self.spark.read.schema(s)")),
            Some(Some(_))
        ));
        // Not a reader chain at all.
        assert_eq!(spark_reader_schema(&expr_of("pd")), None);
        assert_eq!(spark_reader_schema(&expr_of("foo.bar()")), None);
    }

    #[test]
    fn test_should_recognize_spark_session_builders_and_annotations() {
        assert!(is_spark_session_builder(&expr_of(
            "SparkSession.builder.appName(\"x\").config(\"k\", \"v\").getOrCreate()"
        )));
        assert!(is_spark_session_builder(&expr_of(
            "SparkSession.builder.getOrCreate()"
        )));
        assert!(!is_spark_session_builder(&expr_of("make_session()")));
        assert!(!is_spark_session_builder(&expr_of("thing.getOrCreate()")));

        assert!(is_spark_session_annotation(&expr_of("SparkSession")));
        assert!(is_spark_session_annotation(&expr_of(
            "pyspark.sql.SparkSession"
        )));
        assert!(is_spark_session_annotation(&expr_of("\"SparkSession\"")));
        assert!(!is_spark_session_annotation(&expr_of("Session")));
        assert!(!is_spark_session_annotation(&expr_of("1")));
    }

    #[test]
    fn test_should_recognize_spark_read_calls() {
        assert!(is_spark_read_call(&expr_of("spark.read.csv(\"x\")")));
        assert!(is_spark_read_call(&expr_of(
            "spark.read.schema(\"id INT\").parquet(\"x\")"
        )));
        // `.text` is deliberately not a tracked read method, and a bare method call
        // that never touches `.read` is not a reader chain.
        assert!(!is_spark_read_call(&expr_of("spark.read.text(\"x\")")));
        assert!(!is_spark_read_call(&expr_of("obj.csv(\"x\")")));
        assert!(!is_spark_read_call(&expr_of("\"x\"")));
    }

    #[test]
    fn test_should_resolve_spark_select_items_and_their_source_columns() {
        // Bare string items are the only ones reported here -- everything else is
        // validated where it stands, so its `source` is deliberately None.
        let items =
            select_items_of("df.select(\"id\", F.col(\"amount\"), df.region, df[\"tier\"])");
        let outputs: Vec<&str> = items.iter().map(|i| i.output.as_str()).collect();
        assert_eq!(outputs, vec!["id", "amount", "region", "tier"]);
        let sources: Vec<&str> = items.iter().filter_map(|i| i.source.as_deref()).collect();
        assert_eq!(sources, vec!["id"]);
    }

    #[test]
    fn test_should_take_the_alias_as_a_spark_select_items_output_name() {
        let items = select_items_of("df.select(F.col(\"amount\").alias(\"total\"))");
        assert_eq!(items[0].output, "total");
        // The alias is NOT a column of the receiver, so it must never be checked
        // against the receiver's schema.
        assert_eq!(items[0].source, None);
    }

    #[test]
    fn test_should_read_the_spark_select_single_sequence_overload() {
        let items = select_items_of("df.select([\"id\", \"amount\"])");
        let outputs: Vec<&str> = items.iter().map(|i| i.output.as_str()).collect();
        assert_eq!(outputs, vec!["id", "amount"]);
    }

    #[test]
    fn test_should_not_resolve_spark_select_items_it_cannot_name() {
        // A wildcard, an arithmetic expression, a column belonging to some OTHER
        // frame, and a keyword argument each make the whole select unresolvable, so
        // the caller falls back to carrying the receiver's schema forward.
        for source in [
            "df.select(\"*\")",
            "df.select(df[\"a\"] * 2)",
            "df.select(other.a)",
            "df.select(expr(\"a + b\"))",
            "df.select(total=F.col(\"a\"))",
            "df.select()",
        ] {
            assert!(
                extract_spark_select_items(call_of(source), "df").is_none(),
                "source: {source}"
            );
        }
    }

    #[test]
    fn test_should_extract_varargs_drop_columns_for_spark_and_polars() {
        // Spark and polars both spell a multi-column drop as varargs.
        assert_eq!(
            extract_drop_columns(call_of("df.drop(\"a\", \"b\")")),
            Some(vec!["a".to_string(), "b".to_string()])
        );
        assert_eq!(
            extract_drop_columns(call_of("df.drop(F.col(\"a\"), \"b\")")),
            Some(vec!["a".to_string(), "b".to_string()])
        );
    }

    #[test]
    fn test_should_keep_the_single_argument_reading_when_a_later_argument_is_not_a_column() {
        // pandas' deprecated positional-axis form `df.drop("a", 1)` must keep
        // resolving to just `a`, exactly as it did before varargs were read at all.
        assert_eq!(
            extract_drop_columns(call_of("df.drop(\"a\", 1)")),
            Some(vec!["a".to_string()])
        );
        // The pandas kwarg forms are untouched by the varargs reading.
        assert_eq!(
            extract_drop_columns(call_of("df.drop(columns=[\"a\", \"b\"])")),
            Some(vec!["a".to_string(), "b".to_string()])
        );
        assert_eq!(
            extract_drop_columns(call_of("df.drop(\"a\", axis=0)")),
            None
        );
        assert_eq!(extract_drop_columns(call_of("df.drop()")), None);
    }

    #[test]
    fn test_should_extract_dict_string_keys() {
        let Expr::Dict(dict) = expr_of("{\"a\": F.col(\"x\"), \"b\": F.lit(1)}") else {
            panic!("expected a dict literal");
        };
        assert_eq!(
            extract_dict_string_keys(&dict),
            Some(vec!["a".to_string(), "b".to_string()])
        );
        let Expr::Dict(non_literal) = expr_of("{key: 1}") else {
            panic!("expected a dict literal");
        };
        assert_eq!(extract_dict_string_keys(&non_literal), None);
    }
}
