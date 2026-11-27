use super::*;

/// What a call `f(df, ...)` does, given `f`'s definition and its first parameter (after
/// `self`/`cls`). Purely syntactic, so it can be computed for every function in a file
/// before the walk, wherever `f` is defined.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct HelperSummary {
    /// Set when `f` hands its first parameter's frame straight back -- every `return` is a
    /// schema-preserving derivation of it, and so is every reassignment of it: the column
    /// edits made to the returned frame (empty for a pure pass-through).
    pub(crate) returned: Option<Vec<ColumnEdit>>,
    /// Edits that reach the caller's own object because they happen before the parameter
    /// is rebound (`df["z"] = 1` in place, as opposed to on a `df = df.copy()`).
    pub(crate) in_place: Vec<ColumnEdit>,
}

/// An edit that only happens under a condition or loop is `Unknown`: the frame's columns
/// then depend on a branch the checker does not follow. `None` when `f` neither returns
/// its parameter nor edits it in place -- a call to it says nothing about the frame.
pub(crate) fn summarize_helper(func_def: &ast::StmtFunctionDef) -> Option<HelperSummary> {
    let parameters = &func_def.parameters;
    let mut names = parameters
        .posonlyargs
        .iter()
        .chain(parameters.args.iter())
        .map(|p| p.parameter.name.as_str());
    let mut first = names.next()?;
    if matches!(first, "self" | "cls") {
        first = names.next()?;
    }
    // `out = frame.copy()` as the first statement: the rest of the body works on `out`, a
    // different object, so its edits reach the returned frame but not the caller's.
    let body = func_def.body.as_slice();
    let body = match body.first() {
        Some(Stmt::Expr(doc)) if matches!(&*doc.value, Expr::StringLiteral(_)) => &body[1..],
        _ => body,
    };
    let (tracked, alias, body) = match body.split_first() {
        Some((Stmt::Assign(assign), rest)) => match assign.targets.as_slice() {
            [Expr::Name(local)]
                if local.id.as_str() != first && preserves_param(&assign.value, first) =>
            {
                (local.id.as_str(), Alias::No, rest)
            }
            _ => (first, Alias::Yes, body),
        },
        _ => (first, Alias::Yes, body),
    };
    let mut scan = PassthroughScan {
        param: tracked,
        saw_return: false,
        ok: true,
        depth: 0,
        alias,
        edits: Vec::new(),
        in_place: Vec::new(),
    };
    for stmt in body {
        scan.visit_stmt(stmt);
    }
    let returned = (scan.saw_return && scan.ok).then_some(scan.edits);
    (returned.is_some() || !scan.in_place.is_empty()).then_some(HelperSummary {
        returned,
        in_place: scan.in_place,
    })
}

// The column edits a returned chain `param.assign(..).drop(columns=..)` makes to a copy of
// its parameter; `None` as soon as a link is not one the checker follows. A link whose
// meaning depends on the library (`drop("x")` is a row label in pandas) is an unknown edit.
pub(super) fn returned_chain_edits(expr: &Expr, param: &str) -> Option<Vec<ColumnEdit>> {
    match expr {
        Expr::Name(name) => (name.id.as_str() == param).then(Vec::new),
        Expr::Call(call) => {
            let Expr::Attribute(attr) = &*call.func else {
                return None;
            };
            let mut edits = returned_chain_edits(&attr.value, param)?;
            let method = attr.attr.as_str();
            if keyword(call, "inplace").is_some() {
                return None;
            }
            let edit = match method {
                "assign" => Some(
                    ast_extract::assign_output_names(call)
                        .map_or(ColumnEdit::Unknown, ColumnEdit::Add),
                ),
                "with_columns" => Some(
                    ast_extract::with_columns_output_names(call)
                        .map_or(ColumnEdit::Unknown, ColumnEdit::Add),
                ),
                "drop" if keyword(call, "columns").is_some() || keyword(call, "axis").is_some() => {
                    Some(
                        ast_extract::extract_drop_columns(call, false)
                            .map_or(ColumnEdit::Unknown, ColumnEdit::Remove),
                    )
                }
                "rename" if keyword(call, "columns").is_some() => Some(
                    ast_extract::extract_rename_mapping(call)
                        .map_or(ColumnEdit::Unknown, ColumnEdit::Rename),
                ),
                "drop" | "rename" => Some(ColumnEdit::Unknown),
                "select" => Some(
                    ast_extract::extract_select_args_columns(call)
                        .map_or(ColumnEdit::Unknown, ColumnEdit::Replace),
                ),
                _ if ROW_PASSTHROUGH_METHODS.contains(&method)
                    || SCHEMA_PRESERVING_METHODS.contains(&method) =>
                {
                    None
                }
                _ => return None,
            };
            edits.extend(edit);
            Some(edits)
        }
        _ => None,
    }
}

pub(super) fn preserves_param(expr: &Expr, param: &str) -> bool {
    classify_rhs(expr, &|name| name == param)
        .is_some_and(|derived| derived.root == param && derived.kind == Kind::Preserves)
}
