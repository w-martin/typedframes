use super::*;

// Which argument of an `.assign()` / `.with_columns()` call has no readable output name, for the
// explain row.
pub(super) fn unnamed_argument_hint(call: &ast::ExprCall) -> String {
    if call.arguments.keywords.iter().any(|k| k.arg.is_none()) {
        return "a `**` mapping".to_string();
    }
    for arg in &call.arguments.args {
        match arg {
            Expr::Starred(_) => return "a `*` argument".to_string(),
            Expr::List(_) | Expr::Tuple(_) => return "a list of expressions".to_string(),
            Expr::Name(n) => return format!("the variable `{}`", n.id),
            other if ast_extract::pl_expr_output_name(other).is_none() => {
                return "an expression without a literal `.alias()`".to_string()
            }
            _ => {}
        }
    }
    "an argument".to_string()
}

// One `untracked-dataframe` warning per distinct message in a file, with how many more there were:
// the advice is the same for every site, and the sites are listed by `--coverage-detail`.
pub(super) fn collapse_repeated_untracked_warnings(errors: &mut Vec<LintError>) {
    let mut first: HashMap<String, usize> = HashMap::new();
    let mut repeats: HashMap<usize, usize> = HashMap::new();
    let mut drop = Vec::new();
    for (index, e) in errors.iter().enumerate() {
        if e.code != CODE_UNTRACKED_DATAFRAME {
            continue;
        }
        match first.get(&e.message) {
            Some(&kept) => {
                *repeats.entry(kept).or_default() += 1;
                drop.push(index);
            }
            None => {
                first.insert(e.message.clone(), index);
            }
        }
    }
    for (kept, count) in repeats {
        let noun = if count == 1 { "site" } else { "sites" };
        errors[kept].message.push_str(&format!(
            " ({count} more {noun} in this file with the same advice not repeated; `--coverage-detail=explain` lists them)"
        ));
    }
    let mut index = 0;
    errors.retain(|_| {
        index += 1;
        !drop.contains(&(index - 1))
    });
}

// The ranges of every operand of a comparison in `module`.
pub(super) fn compared_ranges(module: &ModModule) -> Vec<ruff_text_size::TextRange> {
    let mut compared = Compared(Vec::new());
    for stmt in &module.body {
        compared.visit_stmt(stmt);
    }
    compared.0
}

pub(super) struct Compared(Vec<ruff_text_size::TextRange>);

impl<'a> Visitor<'a> for Compared {
    fn visit_expr(&mut self, expr: &'a Expr) {
        if let Expr::Compare(compare) = expr {
            self.0.push(compare.left.range());
            self.0.extend(compare.comparators.iter().map(Ranged::range));
        }
        ast_visitor::walk_expr(self, expr);
    }
}

// `frame["c"].notna()`, `frame["c"].str.contains("x")`: methods called on one column, whose
// result is a mask over rows (a selection of columns would be a list of names).
pub(super) fn is_series_method_chain(expr: &Expr) -> bool {
    let mut current = expr;
    let mut called = false;
    loop {
        match current {
            Expr::Call(call) => {
                called = true;
                current = &call.func;
            }
            Expr::Attribute(attr) => current = &attr.value,
            Expr::Subscript(sub) => {
                return called
                    && matches!(&*sub.value, Expr::Name(_))
                    && ast_extract::extract_string_literal(&sub.slice).is_some();
            }
            _ => return false,
        }
    }
}

// A frame method whose result is a frame with columns nothing here can name.
pub(super) fn is_reshaping_call(expr: &Expr) -> bool {
    matches!(expr, Expr::Call(call) if matches!(&*call.func, Expr::Attribute(a)
    if matches!(
        a.attr.as_str(),
        "merge" | "join" | "pivot" | "pivot_table" | "melt" | "set_index" | "reset_index"
            | "select_dtypes" | "stack" | "unstack"
    )))
}

// `pd.concat(...)` / `concat(...)`.
pub(super) fn is_concat_call(expr: &Expr) -> bool {
    let Expr::Call(call) = expr else {
        return false;
    };
    match &*call.func {
        Expr::Name(name) => name.id.as_str() == "concat",
        Expr::Attribute(attr) => attr.attr.as_str() == "concat",
        _ => false,
    }
}

// Whether the call passes any of `names` as a keyword argument.
pub(super) fn has_keyword(call: &ast::ExprCall, names: &[&str]) -> bool {
    call.arguments
        .keywords
        .iter()
        .any(|k| k.arg.as_ref().is_some_and(|a| names.contains(&a.as_str())))
}

// The first plain-name assignment target, used to label a coverage site with the
// variable a reader of the report would actually recognize. Falls back to `fallback`
// for a target with no single name of its own (tuple unpacking, `obj.attr = ...`,
// `d["k"] = ...`), matching what the existing untyped-site call sites already do.
pub(super) fn assign_var_hint(assign: &ast::StmtAssign, fallback: &str) -> String {
    assign
        .targets
        .iter()
        .find_map(|t| {
            if let Expr::Name(n) = t {
                Some(n.id.to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| fallback.to_string())
}
