use super::*;

/// The columns of `left.merge(right, ...)` under pandas' suffix rules: a column both
/// sides carry that is not a shared join key comes out as `<c>_x`/`<c>_y` (or the given
/// `suffixes=`), and the bare name is gone.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Merged {
    Columns(Vec<String>),
    /// The suffixes cannot be read, so which names the overlap becomes is unknown.
    Unknown,
}

enum JoinKeys {
    Known(Vec<String>),
    Unknown,
}

fn shared_join_keys(call: &ast::ExprCall, common: &[&String]) -> JoinKeys {
    let strings = |name: &str| {
        keyword(call, name).and_then(|k| ast_extract::extract_string_list_or_single(&k.value))
    };
    let index_join =
        keyword(call, "left_index").is_some() || keyword(call, "right_index").is_some();
    // `merge_asof(.., by=..)` / `merge_ordered(.., left_by=..)`: the group keys are join keys too.
    let by = |keys: Vec<String>| match strings("by") {
        Some(by) => [keys, by].concat(),
        None if keyword(call, "by").is_some() => Vec::new(),
        None => keys,
    };
    if keyword(call, "on").is_some() {
        return match (
            strings("on"),
            keyword(call, "by").is_some() && strings("by").is_none(),
        ) {
            (Some(on), false) => JoinKeys::Known(by(on)),
            _ => JoinKeys::Unknown,
        };
    }
    if keyword(call, "left_on").is_some() || keyword(call, "right_on").is_some() {
        return match (strings("left_on"), strings("right_on")) {
            (Some(l), Some(r)) if l.len() == r.len() => JoinKeys::Known(by(l
                .into_iter()
                .zip(r)
                .filter(|(a, b)| a == b)
                .map(|(a, _)| a)
                .collect())),
            _ if index_join => JoinKeys::Known(Vec::new()),
            _ => JoinKeys::Unknown,
        };
    }
    let cross = matches!(
        keyword(call, "how").map(|k| &k.value),
        Some(Expr::StringLiteral(s)) if s.value.to_str() == "cross"
    );
    if index_join || cross {
        return JoinKeys::Known(Vec::new());
    }
    // No keys named: pandas joins on the columns the two frames share.
    JoinKeys::Known(common.iter().map(|c| (*c).clone()).collect())
}

/// `Some(None)` is an explicit `None` suffix (the name is left as it is).
fn merge_suffixes(call: &ast::ExprCall) -> Option<(Option<String>, Option<String>)> {
    let Some(suffixes) = keyword(call, "suffixes") else {
        return Some((Some("_x".to_string()), Some("_y".to_string())));
    };
    let elements = match &suffixes.value {
        Expr::Tuple(t) => &t.elts,
        Expr::List(l) => &l.elts,
        _ => return None,
    };
    let [left, right] = elements.as_slice() else {
        return None;
    };
    let one = |e: &Expr| match e {
        Expr::StringLiteral(s) => Some(Some(s.value.to_str().to_string())),
        Expr::NoneLiteral(_) => Some(None),
        _ => None,
    };
    Some((one(left)?, one(right)?))
}

pub(crate) fn merged_columns(left: &[String], right: &[String], call: &ast::ExprCall) -> Merged {
    let common: Vec<&String> = left.iter().filter(|c| right.contains(c)).collect();
    let keys = shared_join_keys(call, &common);
    let overlap: Vec<&String> = match &keys {
        JoinKeys::Known(keys) => common
            .iter()
            .copied()
            .filter(|c| !keys.contains(c))
            .collect(),
        JoinKeys::Unknown => common.clone(),
    };
    if overlap.is_empty() {
        let mut columns = left.to_vec();
        columns.extend(right.iter().filter(|c| !left.contains(c)).cloned());
        return Merged::Columns(columns);
    }
    let Some((left_suffix, right_suffix)) = merge_suffixes(call) else {
        return Merged::Unknown;
    };
    let with = |name: &String, suffix: &Option<String>| match suffix {
        Some(suffix) => format!("{name}{suffix}"),
        None => name.clone(),
    };
    let JoinKeys::Known(keys) = keys else {
        // Any common column may be a key (bare) or an overlap (suffixed): admit both.
        let mut columns = left.to_vec();
        columns.extend(right.iter().filter(|c| !left.contains(c)).cloned());
        for name in overlap {
            columns.push(with(name, &left_suffix));
            columns.push(with(name, &right_suffix));
        }
        return Merged::Columns(columns);
    };
    let mut columns: Vec<String> = left
        .iter()
        .map(|c| {
            if overlap.contains(&c) {
                with(c, &left_suffix)
            } else {
                c.clone()
            }
        })
        .collect();
    for c in right
        .iter()
        .filter(|c| !keys.contains(c) || !left.contains(c))
    {
        columns.push(if overlap.contains(&c) {
            with(c, &right_suffix)
        } else {
            c.clone()
        });
    }
    Merged::Columns(columns)
}
