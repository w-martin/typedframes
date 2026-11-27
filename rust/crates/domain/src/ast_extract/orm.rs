use super::*;

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
