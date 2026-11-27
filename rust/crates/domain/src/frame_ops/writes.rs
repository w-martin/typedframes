use super::*;

// `f(df, ...)`, `module.f(df, ...)`, `pd.concat([df, other])`: the result's columns
// are whatever the callee produces. A first argument that is itself not a frame
// (`f(df["a"])`) says nothing about a frame result, so it does not count.
pub(super) fn first_arg_root(
    call: &ast::ExprCall,
    is_tracked: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let tracked_name = |expr: &Expr| match expr {
        Expr::Name(name) if is_tracked(name.id.as_str()) => Some(name.id.to_string()),
        _ => None,
    };
    let first = call.arguments.args.first()?;
    match first {
        Expr::List(list) => list.elts.iter().find_map(tracked_name),
        Expr::Tuple(tuple) => tuple.elts.iter().find_map(tracked_name),
        other => classify_rhs(other, is_tracked)
            .filter(|derived| derived.kind != Kind::NotAFrame)
            .map(|derived| derived.root),
    }
}

pub(super) fn classify_subscript(
    subscript: &ast::ExprSubscript,
    is_tracked: &dyn Fn(&str) -> bool,
) -> Option<Derivation> {
    if let Expr::Attribute(indexer) = &*subscript.value {
        let name = indexer.attr.as_str();
        if matches!(name, "loc" | "iloc" | "at" | "iat") {
            let receiver = classify_rhs(&indexer.value, is_tracked)?;
            let kind = if receiver.kind == Kind::NotAFrame || matches!(name, "at" | "iat") {
                Kind::NotAFrame
            } else {
                match (indexer_kind(&subscript.slice), receiver.kind) {
                    (Kind::Preserves, Kind::Derived) => Kind::Derived,
                    (kind, _) => kind,
                }
            };
            return Some(Derivation {
                root: receiver.root,
                kind,
            });
        }
    }

    let receiver = classify_rhs(&subscript.value, is_tracked)?;
    if receiver.kind == Kind::NotAFrame {
        return Some(receiver);
    }
    let kind = match &*subscript.slice {
        Expr::StringLiteral(_) => Kind::NotAFrame,
        Expr::List(_) => Kind::Derived,
        // A boolean mask or row slice keeps the columns; any other slice expression is
        // treated the same way, matching how the assignment dispatch already handles
        // `df[<not a literal list>]`.
        _ if receiver.kind == Kind::Preserves => Kind::Preserves,
        _ => Kind::Derived,
    };
    Some(Derivation {
        root: receiver.root,
        kind,
    })
}

// `.loc[...]` / `.iloc[...]`: a single indexer selects rows and keeps every column;
// `(rows, cols)` keeps them only when `cols` is a bare `:`.
fn indexer_kind(slice: &Expr) -> Kind {
    let scalar = |expr: &Expr| matches!(expr, Expr::StringLiteral(_) | Expr::NumberLiteral(_));
    match slice {
        Expr::Tuple(tuple) if tuple.elts.len() == 2 => match &tuple.elts[1] {
            Expr::Slice(full)
                if full.lower.is_none() && full.upper.is_none() && full.step.is_none() =>
            {
                if scalar(&tuple.elts[0]) {
                    Kind::NotAFrame
                } else {
                    Kind::Preserves
                }
            }
            column if scalar(column) => Kind::NotAFrame,
            _ => Kind::Derived,
        },
        rows if scalar(rows) => Kind::NotAFrame,
        _ => Kind::Preserves,
    }
}

// The receiver and columns of a subscript write into a frame: `df["c"] = ...`,
// `df[["a", "b"]] = ...`, `df.loc[rows, "c"] = ...`, `df.loc[rows, ["a", "b"]] = ...`.
pub(crate) fn column_write_target(target: &Expr) -> Option<(String, Vec<String>)> {
    let Expr::Subscript(subscript) = target else {
        return None;
    };
    let columns_of = |expr: &Expr| {
        ast_extract::extract_string_literal(expr)
            .map(|c| vec![c.to_string()])
            .or_else(|| ast_extract::extract_string_list(expr))
    };
    match &*subscript.value {
        Expr::Name(recv) => Some((recv.id.to_string(), columns_of(&subscript.slice)?)),
        Expr::Attribute(indexer) if indexer.attr.as_str() == "loc" => {
            let Expr::Name(recv) = &*indexer.value else {
                return None;
            };
            let Expr::Tuple(indexers) = &*subscript.slice else {
                return None;
            };
            let [_, columns] = indexers.elts.as_slice() else {
                return None;
            };
            Some((recv.id.to_string(), columns_of(columns)?))
        }
        _ => None,
    }
}

/// The frame `frame[key] = ..` / `frame.loc[rows, key] = ..` writes a column of under a name only
/// the running program knows (an f-string, a loop variable, a call). A boolean row mask is not
/// told apart from a variable key, so this over-approximates: the frame is merely left open.
pub(crate) fn dynamic_column_write_receiver(target: &Expr) -> Option<(String, &Expr)> {
    let Expr::Subscript(subscript) = target else {
        return None;
    };
    let is_computed = |expr: &Expr| {
        matches!(
            expr,
            Expr::FString(_) | Expr::Name(_) | Expr::Call(_) | Expr::BinOp(_) | Expr::Subscript(_)
        )
    };
    match &*subscript.value {
        Expr::Name(recv) => {
            is_computed(&subscript.slice).then(|| (recv.id.to_string(), &*subscript.slice))
        }
        Expr::Attribute(indexer) if indexer.attr.as_str() == "loc" => {
            let (Expr::Name(recv), Expr::Tuple(indexers)) = (&*indexer.value, &*subscript.slice)
            else {
                return None;
            };
            let [_, columns] = indexers.elts.as_slice() else {
                return None;
            };
            is_computed(columns).then(|| (recv.id.to_string(), columns))
        }
        _ => None,
    }
}

/// The frame `frame.loc[row] = ..` / `frame.at[row, col] = ..` writes a row of: values that may name
/// columns the frame does not have yet.
pub(crate) fn row_insert_receiver(target: &Expr) -> Option<String> {
    let Expr::Subscript(subscript) = target else {
        return None;
    };
    let Expr::Attribute(indexer) = &*subscript.value else {
        return None;
    };
    let Expr::Name(recv) = &*indexer.value else {
        return None;
    };
    let single_row = !matches!(&*subscript.slice, Expr::Tuple(_));
    ((indexer.attr.as_str() == "loc" && single_row) || indexer.attr.as_str() == "at")
        .then(|| recv.id.to_string())
}

pub(crate) fn keyword<'c>(call: &'c ast::ExprCall, name: &str) -> Option<&'c ast::Keyword> {
    call.arguments
        .keywords
        .iter()
        .find(|k| k.arg.as_ref().map(|a| a.as_str()) == Some(name))
}

// The literal value of `inplace=`, or `None` when it is absent or not a literal.
pub(crate) fn inplace_literal(call: &ast::ExprCall) -> Option<bool> {
    match &keyword(call, "inplace")?.value {
        Expr::BooleanLiteral(b) => Some(b.value),
        _ => None,
    }
}

// What an in-place `drop`/`rename` does to a frame's columns.
pub(crate) enum InplaceColumns<T> {
    Edit(T),
    Unchanged,
    Unknown,
}

pub(crate) enum Axis {
    Unspecified,
    Rows,
    Columns,
    Dynamic,
}

pub(crate) fn axis_of(call: &ast::ExprCall) -> Axis {
    let Some(axis) = keyword(call, "axis") else {
        return Axis::Unspecified;
    };
    let is = |int: u64, name: &str| match &axis.value {
        Expr::NumberLiteral(n) => {
            matches!(&n.value, ast::Number::Int(i) if i.as_u64() == Some(int))
        }
        Expr::StringLiteral(s) => s.value.to_str() == name,
        _ => false,
    };
    if is(0, "index") {
        Axis::Rows
    } else if is(1, "columns") {
        Axis::Columns
    } else {
        Axis::Dynamic
    }
}

// `inplace=` only exists in pandas, where `drop`'s positional labels are rows unless
// `axis=1`/`columns=` says otherwise.
pub(crate) fn inplace_drop_columns(call: &ast::ExprCall) -> InplaceColumns<Vec<String>> {
    let from = |expr: &Expr| {
        ast_extract::extract_string_list_or_single(expr)
            .map_or(InplaceColumns::Unknown, InplaceColumns::Edit)
    };
    if keyword(call, "level").is_some() {
        return InplaceColumns::Unknown;
    }
    if let Some(columns) = keyword(call, "columns") {
        return from(&columns.value);
    }
    match axis_of(call) {
        Axis::Unspecified | Axis::Rows => InplaceColumns::Unchanged,
        Axis::Columns => call
            .arguments
            .args
            .first()
            .or_else(|| keyword(call, "labels").map(|k| &k.value))
            .map_or(InplaceColumns::Unknown, from),
        Axis::Dynamic => InplaceColumns::Unknown,
    }
}

// As for `drop`, a positional mapper renames row labels unless `axis=1`/`columns=` says
// otherwise.
pub(crate) fn inplace_rename_mapping(
    call: &ast::ExprCall,
) -> InplaceColumns<HashMap<String, String>> {
    let from = |expr: &Expr| match expr {
        Expr::Dict(dict) => ast_extract::extract_string_dict(dict)
            .map_or(InplaceColumns::Unknown, InplaceColumns::Edit),
        _ => InplaceColumns::Unknown,
    };
    if let Some(columns) = keyword(call, "columns") {
        return from(&columns.value);
    }
    match axis_of(call) {
        Axis::Unspecified | Axis::Rows => InplaceColumns::Unchanged,
        Axis::Columns => call
            .arguments
            .args
            .first()
            .or_else(|| keyword(call, "mapper").map(|k| &k.value))
            .map_or(InplaceColumns::Unknown, from),
        Axis::Dynamic => InplaceColumns::Unknown,
    }
}
