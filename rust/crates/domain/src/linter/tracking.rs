use super::*;

// The local names and parameters of `func` that are certainly an instance of a class, by
// class name: a parameter annotated `Name` that is never rebound, or a name bound exactly
// once, by `name = Class(...)`. Which class a name stands for is all this decides; whether
// that class is known, and what its methods return, is for the caller to look up. A rebinding
// anywhere in the function, nested scopes included, rules the name out.
pub(super) fn receiver_classes(func: &ast::StmtFunctionDef) -> HashMap<String, String> {
    let mut bindings = Bindings::default();
    let mut annotated: HashMap<String, String> = HashMap::new();
    let parameters = &func.parameters;
    for parameter in parameters
        .posonlyargs
        .iter()
        .chain(&parameters.args)
        .chain(&parameters.kwonlyargs)
    {
        bindings.bind(parameter.parameter.name.as_str());
        if let Some(Expr::Name(class)) = parameter.parameter.annotation.as_deref() {
            annotated.insert(parameter.parameter.name.to_string(), class.id.to_string());
        }
    }
    for rest in [&parameters.vararg, &parameters.kwarg]
        .into_iter()
        .flatten()
    {
        bindings.bind(rest.name.as_str());
    }
    for stmt in &func.body {
        bindings.visit_stmt(stmt);
    }

    annotated
        .into_iter()
        .chain(bindings.constructed.clone())
        .filter(|(name, _)| bindings.counts.get(name) == Some(&1))
        .collect()
}

#[derive(Default)]
struct Bindings {
    pub(super) counts: HashMap<String, usize>,
    pub(super) constructed: HashMap<String, String>,
}

impl Bindings {
    pub(super) fn bind(&mut self, name: &str) {
        *self.counts.entry(name.to_string()).or_default() += 1;
    }

    fn note_constructed_instance(&mut self, assign: &ruff_python_ast::StmtAssign) {
        if let ([Expr::Name(target)], Expr::Call(call)) =
            (assign.targets.as_slice(), &*assign.value)
        {
            if let Expr::Name(class) = &*call.func {
                self.constructed
                    .insert(target.id.to_string(), class.id.to_string());
            }
        }
    }
}

impl<'a> Visitor<'a> for Bindings {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::Assign(assign) => {
                self.note_constructed_instance(assign);
            }
            Stmt::FunctionDef(f) => self.bind(f.name.as_str()),
            Stmt::ClassDef(c) => self.bind(c.name.as_str()),
            Stmt::Import(i) => {
                for alias in &i.names {
                    let name = alias.asname.as_ref().unwrap_or(&alias.name);
                    self.bind(name.as_str().split('.').next().unwrap_or_default());
                }
            }
            Stmt::ImportFrom(i) => {
                for alias in &i.names {
                    self.bind(alias.asname.as_ref().unwrap_or(&alias.name).as_str());
                }
            }
            // A name another scope may rebind, however it is reached.
            Stmt::Global(g) => g.names.iter().for_each(|n| {
                self.bind(n.as_str());
                self.bind(n.as_str());
            }),
            Stmt::Nonlocal(g) => g.names.iter().for_each(|n| {
                self.bind(n.as_str());
                self.bind(n.as_str());
            }),
            _ => {}
        }
        ast_visitor::walk_stmt(self, stmt);
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        if let Expr::Name(name) = expr {
            if !name.ctx.is_load() {
                self.bind(name.id.as_str());
            }
        }
        ast_visitor::walk_expr(self, expr);
    }

    fn visit_pattern(&mut self, pattern: &'a ast::Pattern) {
        match pattern {
            ast::Pattern::MatchAs(p) => {
                if let Some(name) = &p.name {
                    self.bind(name.as_str());
                }
            }
            ast::Pattern::MatchStar(p) => {
                if let Some(name) = &p.name {
                    self.bind(name.as_str());
                }
            }
            ast::Pattern::MatchMapping(p) => {
                if let Some(rest) = &p.rest {
                    self.bind(rest.as_str());
                }
            }
            _ => {}
        }
        ast_visitor::walk_pattern(self, pattern);
    }

    fn visit_except_handler(&mut self, handler: &'a ast::ExceptHandler) {
        let ast::ExceptHandler::ExceptHandler(h) = handler;
        if let Some(name) = &h.name {
            self.bind(name.as_str());
        }
        ast_visitor::walk_except_handler(self, handler);
    }

    fn visit_parameter(&mut self, parameter: &'a ast::Parameter) {
        self.bind(parameter.name.as_str());
        ast_visitor::walk_parameter(self, parameter);
    }
}

// What `self.<attr> = <value>(...)` in `__init__` resolved `<attr>` to -- see
// `Linter::current_self_attrs`'s doc comment for why this exists at all.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SelfAttrOrigin {
    // Class name exactly as spelled at the assignment site (`ClassName` or
    // `module.ClassName`'s trailing segment) -- looked up in `Linter::class_methods`
    // (same-file) or the project index's cross-file `class_methods` (see index.rs) to
    // resolve `self.<attr>.<method>(...)` to a schema.
    pub(crate) class_name: String,
    // `name`/`is_attribute_call` in exactly the shape
    // `UnresolvedDataframeShapedCall` already uses, for feeding into that SAME
    // `imports`/`module_aliases` resolution in index.rs when `class_name` isn't
    // resolvable same-file (external package, or a first-party class not yet
    // extended to cross-file `class_methods`): `resolve_name` is `class_name` itself
    // (resolved via `imports`, i.e. `from module import ClassName`) when the
    // assignment was `self.attr = ClassName(...)`, or the qualifying module name
    // (resolved via `module_aliases`, i.e. `import module`) when it was
    // `self.attr = module.ClassName(...)`.
    pub(crate) resolve_name: String,
    pub(crate) is_attribute_call: bool,
}

// A candidate pass-through-count reversal, recorded when a function's return schema
// was inferred from a bare `return <var>` (see the `Stmt::FunctionDef` body-inference
// arm) rather than removed immediately. `var`/`line` identify the exact
// `typed_sites`/`untyped_sites` entry to remove -- see `Linter::pending_passthrough_reversals`.
#[derive(Debug, Clone)]
pub(crate) struct PendingPassthroughReversal {
    pub(crate) var: String,
    pub(crate) line: usize,
}

/// AST visitor that tracks DataFrame schemas and validates column access.
///
/// # State model
///
/// The linter maintains three pieces of mutable state as it walks the AST:
///
/// * `schemas` — maps a schema name to its list of known column names.  Schemas are
///   created from `BaseSchema` class definitions, inferred from `usecols=`/`columns=`
///   arguments, and synthesised for intermediate results of method chains
///   (e.g. `df.drop(...)`, `df.rename(...)`).  Inferred schema names are prefixed with
///   `__inferred_` to distinguish them from user-defined ones in error messages.
///
/// * `variables` — maps a variable name to `(schema_name, line_defined)`.  Updated
///   whenever a variable is assigned a DataFrame value.  The `line_defined` is used in
///   error messages to tell the user where the schema was established.
///
/// * `functions` — maps a function name to the schema name it returns, populated from
///   return-type annotations (`-> Annotated[pd.DataFrame, MySchema]`).  Used when a
///   call result is assigned to a new variable.
///
/// # Visitor pattern
///
/// `visit_stmt` handles statement-level nodes (class definitions, assignments, function
/// definitions, `del` statements).  `visit_expr` handles expression-level column access
/// checks (`df["col"]`, `df.col`, `pl.col("col")`).  Both methods recurse into child
/// nodes manually rather than using a trait-based visitor, keeping control flow explicit.
// The result of a polars frame operation the checker can follow: the columns it reads
// from each frame (frame variable, column names) and the columns it produces.
pub(super) struct PolarsJoin {
    pub(super) left_keys: Vec<String>,
    pub(super) right_keys: Vec<String>,
    pub(super) columns: Vec<String>,
}

pub(super) struct PolarsFrameOp {
    pub(super) reads: Vec<(String, Vec<String>)>,
    pub(super) columns: Vec<String>,
}

pub(super) fn imports_only_polars(body: &[Stmt]) -> bool {
    let imports = |module: &str| {
        body.iter().any(|stmt| match stmt {
            Stmt::Import(import) => import
                .names
                .iter()
                .any(|a| a.name.as_str().split('.').next() == Some(module)),
            Stmt::ImportFrom(import) => import
                .module
                .as_ref()
                .is_some_and(|m| m.as_str().split('.').next() == Some(module)),
            _ => false,
        })
    };
    imports("polars") && !imports("pandas")
}

/// Where a column was first read: the statement counter, line and column.
pub(super) type FirstRead = (usize, usize, usize);

/// A DataFrame the checker knows to be one but not the columns of, with the columns it knows
/// the frame certainly does not have: a name dropped, renamed away or replaced since its
/// load. Only unconditional edits are recorded, so each entry is a fact about every path.
#[derive(Clone, Default)]
pub(super) struct OpenFrame {
    pub(super) depth: usize,
    // Column -> why it is gone and the line that removed it.
    pub(super) absent: HashMap<String, (String, usize)>,
    // Columns assigned (or read off a frame that already had them) at any point so far.
    pub(super) written: std::collections::HashSet<String>,
}

pub(super) struct LoopBinding {
    pub(super) names: Vec<String>,
    pub(super) rows: Vec<Vec<static_eval::Value>>,
}
