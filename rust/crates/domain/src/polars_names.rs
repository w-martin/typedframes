//! Which names in a file hold polars frames, so a method both libraries have (`drop`, `rename`,
//! `join`, `melt`) is read the polars way on them even when the file also imports pandas.

use std::collections::{HashMap, HashSet};

use ruff_python_ast::visitor::{self as ast_visitor, Visitor};
use ruff_python_ast::{Expr, Stmt};

const POLARS_MODULES: &[&str] = &["pl", "polars"];

/// A value the file builds with polars, or derives from a name it knows holds a polars frame.
fn is_polars_value(expr: &Expr, known: &HashSet<String>) -> bool {
    match expr {
        Expr::Name(name) => known.contains(name.id.as_str()),
        Expr::Subscript(sub) => is_polars_value(&sub.value, known),
        Expr::Call(call) => match &*call.func {
            Expr::Attribute(attr) => {
                if matches!(&*attr.value, Expr::Name(m) if POLARS_MODULES.contains(&m.id.as_str()))
                {
                    return true;
                }
                // Leaving polars: `to_pandas()`, `to_numpy()`, `to_dict()`, ...
                !attr.attr.as_str().starts_with("to_") && is_polars_value(&attr.value, known)
            }
            _ => false,
        },
        _ => false,
    }
}

fn is_polars_annotation(annotation: &Expr) -> bool {
    match annotation {
        Expr::Attribute(attr) => {
            matches!(&*attr.value, Expr::Name(m) if POLARS_MODULES.contains(&m.id.as_str()))
                && matches!(attr.attr.as_str(), "DataFrame" | "LazyFrame")
        }
        Expr::StringLiteral(s) => {
            matches!(s.value.to_str(), "pl.DataFrame" | "pl.LazyFrame")
        }
        _ => false,
    }
}

#[derive(Default)]
struct Bindings<'a> {
    // Name -> the value of each plain assignment, `None` for any other way of binding it.
    by_name: HashMap<String, Vec<Option<&'a Expr>>>,
    polars_params: HashSet<String>,
}

impl<'a> Bindings<'a> {
    fn other(&mut self, target: &Expr) {
        match target {
            Expr::Name(n) => self.by_name.entry(n.id.to_string()).or_default().push(None),
            Expr::Tuple(t) => t.elts.iter().for_each(|e| self.other(e)),
            Expr::List(l) => l.elts.iter().for_each(|e| self.other(e)),
            Expr::Starred(s) => self.other(&s.value),
            _ => {}
        }
    }
}

impl<'a> Visitor<'a> for Bindings<'a> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::Assign(assign) => match assign.targets.as_slice() {
                [Expr::Name(n)] => self
                    .by_name
                    .entry(n.id.to_string())
                    .or_default()
                    .push(Some(&assign.value)),
                targets => targets.iter().for_each(|t| self.other(t)),
            },
            Stmt::AnnAssign(ann) => match (&*ann.target, ann.value.as_deref()) {
                (Expr::Name(n), Some(value)) => self
                    .by_name
                    .entry(n.id.to_string())
                    .or_default()
                    .push(Some(value)),
                (target, _) => self.other(target),
            },
            Stmt::AugAssign(aug) => self.other(&aug.target),
            Stmt::For(f) => self.other(&f.target),
            Stmt::With(w) => w
                .items
                .iter()
                .filter_map(|i| i.optional_vars.as_deref())
                .for_each(|v| self.other(v)),
            Stmt::FunctionDef(f) => {
                let parameters = &f.parameters;
                for p in parameters
                    .posonlyargs
                    .iter()
                    .chain(parameters.args.iter())
                    .chain(parameters.kwonlyargs.iter())
                {
                    let name = p.parameter.name.id.to_string();
                    if p.parameter
                        .annotation
                        .as_deref()
                        .is_some_and(is_polars_annotation)
                    {
                        self.polars_params.insert(name);
                    } else {
                        self.by_name.entry(name).or_default().push(None);
                    }
                }
            }
            _ => {}
        }
        ast_visitor::walk_stmt(self, stmt);
    }
}

/// The names every binding of which, anywhere in the file, is a polars frame.
pub(crate) fn polars_names(body: &[Stmt]) -> HashSet<String> {
    let mut bindings = Bindings::default();
    for stmt in body {
        bindings.visit_stmt(stmt);
    }
    // A parameter annotated as polars counts for the name only if no other binding disagrees.
    let mut known: HashSet<String> = bindings.polars_params.clone();
    let mut changed = true;
    while changed {
        changed = false;
        for (name, values) in &bindings.by_name {
            // `ev = ev.unique()` keeps a polars `ev` polars, given one binding that makes it so.
            let mut with_self = known.clone();
            with_self.insert(name.clone());
            let all_polars = values
                .iter()
                .all(|v| v.is_some_and(|expr| is_polars_value(expr, &with_self)))
                && values
                    .iter()
                    .any(|v| v.is_some_and(|expr| is_polars_value(expr, &known)));
            let in_known = known.contains(name);
            if all_polars && !values.is_empty() && !in_known {
                known.insert(name.clone());
                changed = true;
            } else if !all_polars && in_known && !bindings.polars_params.contains(name) {
                known.remove(name);
                changed = true;
            }
        }
        // A polars-annotated parameter that is also bound some other way is not one name.
        for name in bindings.polars_params.iter() {
            if bindings
                .by_name
                .get(name)
                .is_some_and(|values| values.iter().any(|v| v.is_none()))
                && known.remove(name)
            {
                changed = true;
            }
        }
    }
    known
}

/// Whether `expr` is a polars frame: built with polars, or a name that always holds one.
pub(crate) fn is_polars_expr(expr: &Expr, names: &HashSet<String>) -> bool {
    is_polars_value(expr, names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruff_python_parser::parse_module;

    fn names(source: &str) -> HashSet<String> {
        polars_names(&parse_module(source).unwrap().into_syntax().body)
    }

    #[test]
    fn test_should_know_a_name_built_with_polars_and_the_names_derived_from_it() {
        let found = names("import polars as pl\nk = pl.read_parquet('a')\nh = k.drop('a', 'b')\n");

        assert!(found.contains("k") && found.contains("h"));
    }

    #[test]
    fn test_should_not_know_a_name_bound_to_pandas_somewhere_else() {
        let found = names(
            "import pandas as pd\nimport polars as pl\nk = pl.read_parquet('a')\nk = pd.read_csv('b')\n",
        );

        assert!(!found.contains("k"));
    }

    #[test]
    fn test_should_know_a_name_rebound_from_itself_in_a_file_that_imports_pandas() {
        let found = names(
            "import pandas as pd\nimport polars as pl\nev = pl.scan_parquet('a')\nev = ev.unique(subset=['id'])\n",
        );

        assert!(found.contains("ev"));
    }

    #[test]
    fn test_should_not_know_a_name_only_ever_rebound_from_itself() {
        let found = names("def f(ev):\n    ev = ev.unique()\n    return ev\n");

        assert!(!found.contains("ev"));
    }

    #[test]
    fn test_should_leave_polars_at_to_pandas() {
        let found = names("import polars as pl\nk = pl.read_parquet('a')\np = k.to_pandas()\n");

        assert!(found.contains("k") && !found.contains("p"));
    }

    #[test]
    fn test_should_know_a_parameter_annotated_as_a_polars_frame() {
        let found = names("import polars as pl\n\n\ndef f(lf: pl.LazyFrame):\n    return lf\n");

        assert!(found.contains("lf"));
    }
}
