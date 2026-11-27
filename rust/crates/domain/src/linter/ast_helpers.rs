use super::*;

// `DataFrame(...)` or `<anything>.DataFrame(...)` -- the constructor shape
// `dict_list_var_candidates`/`extract_load_columns` both recognize, matching how the
// rest of the checker treats a `DataFrame` call regardless of its receiver (`pd.`,
// `pl.`, an aliased import, ...).
pub(super) fn is_dataframe_constructor_callee(func: &Expr) -> bool {
    match func {
        Expr::Name(name) => name.id.as_str() == "DataFrame",
        Expr::Attribute(attr) => attr.attr.as_str() == "DataFrame",
        _ => false,
    }
}

// A call to anything in `LOAD_FUNCTIONS` (every `read_*`/`scan_*` loader and
// `DataFrame`), by bare name or as an attribute of anything -- the callees whose
// `columns=`/`usecols=` keyword `extract_load_columns` reads.
pub(super) fn is_load_callee(func: &Expr) -> bool {
    match func {
        Expr::Name(name) => LOAD_FUNCTIONS.contains(&name.id.as_str()),
        Expr::Attribute(attr) => LOAD_FUNCTIONS.contains(&attr.attr.as_str()),
        _ => false,
    }
}

// Names standing for rows (`DataFrame(name)`): an append accumulator or a literal whose
// keys are literal.
pub(super) fn collect_row_var_candidates(body: &[Stmt]) -> HashMap<String, Vec<String>> {
    let mut collector = DictListBindingCollector {
        bindings: HashMap::new(),
        accounted_for: std::collections::HashSet::new(),
    };
    for stmt in body {
        collector.visit_stmt(stmt);
    }
    let mut rows = HashMap::new();
    for (name, binding) in collector.bindings {
        if let DictListBinding::Building(keys) | DictListBinding::Rows(keys) = binding {
            if !keys.is_empty() {
                rows.insert(name, keys);
            }
        }
    }
    rows
}

// The row indexer of a `df.loc[rows, cols]` target.
pub(super) fn loc_row_indexer(target: &Expr) -> Option<&Expr> {
    let Expr::Subscript(subscript) = target else {
        return None;
    };
    let Expr::Attribute(indexer) = &*subscript.value else {
        return None;
    };
    if indexer.attr.as_str() != "loc" {
        return None;
    }
    let Expr::Tuple(indexers) = &*subscript.slice else {
        return None;
    };
    indexers.elts.first()
}

pub(super) fn collect_name_targets(
    target: &Expr,
    why: &'static str,
    out: &mut Vec<(String, &'static str)>,
) {
    match target {
        Expr::Name(name) => out.push((name.id.to_string(), why)),
        Expr::Tuple(tuple) => tuple
            .elts
            .iter()
            .for_each(|e| collect_name_targets(e, why, out)),
        Expr::List(list) => list
            .elts
            .iter()
            .for_each(|e| collect_name_targets(e, why, out)),
        Expr::Starred(starred) => collect_name_targets(&starred.value, why, out),
        _ => {}
    }
}

// The expressions a statement evaluates itself, excluding those in nested bodies (each
// nested statement is visited on its own).
// Builtins that cannot reach a frame or object they are not handed.
pub(super) const HARMLESS_CALLEES: &[&str] = &[
    "print",
    "len",
    "type",
    "isinstance",
    "list",
    "set",
    "tuple",
    "dict",
    "frozendict",
    "sorted",
    "str",
    "repr",
    "display",
    "id",
    "min",
    "max",
    "sum",
    "any",
    "all",
    "bool",
    "int",
    "float",
    "enumerate",
    "zip",
    "iter",
    "next",
    "hash",
    "vars",
    "dir",
];

// `reset_index(drop=True)`: the index is discarded, not turned into columns.
pub(super) fn resets_index_dropping(call: &ast::ExprCall) -> bool {
    keyword(call, "drop").is_some_and(|k| matches!(&k.value, Expr::BooleanLiteral(b) if b.value))
}

// The `index=` of a pandas `DataFrame(..)` constructor call.
pub(super) fn constructor_index(call: &ast::ExprCall) -> Option<&Expr> {
    let is_constructor = match &*call.func {
        Expr::Attribute(a) => {
            a.attr.as_str() == "DataFrame"
                && matches!(&*a.value, Expr::Name(n) if matches!(n.id.as_str(), "pd" | "pandas"))
        }
        Expr::Name(n) => n.id.as_str() == "DataFrame",
        _ => false,
    };
    is_constructor
        .then(|| keyword(call, "index").map(|k| &k.value))
        .flatten()
}

// The level names an index expression gives a frame: none for a literal list, the literal
// `name=`/`names=` of an index constructor, otherwise unknown (`None`).
pub(super) fn index_name_of(index: &Expr) -> Option<Vec<String>> {
    match index {
        Expr::List(_) | Expr::Tuple(_) => Some(Vec::new()),
        Expr::Call(call) => {
            let names = keyword(call, "name").or_else(|| keyword(call, "names"))?;
            ast_extract::extract_string_list_or_single(&names.value)
        }
        _ => None,
    }
}

// `df` in `df.index`.
pub(super) fn index_owner(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Attribute(a) if a.attr.as_str() == "index" => match &*a.value {
            Expr::Name(n) => Some(n.id.as_str()),
            _ => None,
        },
        _ => None,
    }
}

// `obj.attr = ..` on anything but `self` may store through an alias of `self`.
pub(super) fn stores_to_foreign_attribute(target: &Expr) -> bool {
    match target {
        Expr::Attribute(a) => !matches!(&*a.value, Expr::Name(n) if n.id.as_str() == "self"),
        Expr::Tuple(t) => t.elts.iter().any(stores_to_foreign_attribute),
        Expr::List(l) => l.elts.iter().any(stores_to_foreign_attribute),
        Expr::Starred(s) => stores_to_foreign_attribute(&s.value),
        _ => false,
    }
}

pub(super) fn own_expressions(stmt: &Stmt) -> Vec<&Expr> {
    match stmt {
        Stmt::Expr(s) => vec![&*s.value],
        Stmt::Assign(s) => vec![&*s.value],
        Stmt::AnnAssign(s) => s.value.as_deref().into_iter().collect(),
        Stmt::AugAssign(s) => vec![&*s.value],
        Stmt::Return(s) => s.value.as_deref().into_iter().collect(),
        Stmt::If(s) => std::iter::once(&*s.test)
            .chain(s.elif_else_clauses.iter().filter_map(|c| c.test.as_ref()))
            .collect(),
        Stmt::While(s) => vec![&*s.test],
        Stmt::For(s) => vec![&*s.iter],
        Stmt::With(s) => s.items.iter().map(|item| &item.context_expr).collect(),
        _ => Vec::new(),
    }
}

// Collects the target of every assignment expression (`(df := ...)`) it visits.
pub(super) struct WalrusCollector {
    pub(super) names: Vec<String>,
}

impl<'a> Visitor<'a> for WalrusCollector {
    fn visit_expr(&mut self, expr: &'a Expr) {
        if let Expr::Named(named) = expr {
            if let Expr::Name(target) = &*named.target {
                self.names.push(target.id.to_string());
            }
        }
        ast_visitor::walk_expr(self, expr);
    }
}

// Names a function body declares `global` (not descending into nested scopes).
pub(super) fn global_names(body: &[Stmt]) -> Vec<String> {
    let mut collector = GlobalCollector(Vec::new());
    for stmt in body {
        collector.visit_stmt(stmt);
    }
    collector.0
}

struct GlobalCollector(Vec<String>);

impl<'a> Visitor<'a> for GlobalCollector {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::Global(global) => self.0.extend(global.names.iter().map(|n| n.to_string())),
            Stmt::FunctionDef(_) | Stmt::ClassDef(_) => {}
            _ => ast_visitor::walk_stmt(self, stmt),
        }
    }
}

// Names of the classes `body` defines (also inside classes) and of everything it imports.
pub(super) fn collect_class_and_imported_names<'a>(
    body: &'a [Stmt],
    out: &mut std::collections::HashSet<&'a str>,
) {
    for stmt in body {
        match stmt {
            Stmt::ClassDef(class) => {
                out.insert(class.name.as_str());
                collect_class_and_imported_names(&class.body, out);
            }
            Stmt::ImportFrom(import) => {
                for alias in &import.names {
                    out.insert(alias.name.as_str());
                }
            }
            Stmt::If(if_stmt) => collect_class_and_imported_names(&if_stmt.body, out),
            _ => {}
        }
    }
}
