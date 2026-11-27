use super::*;

// Every function and method of a file with its column contract (`None` when its definition
// is repeated with a different one, or decorated in a way that may change the call).
pub(super) struct ContractCollector<'l> {
    pub(super) linter: &'l Linter,
    pub(super) class: Option<String>,
    pub(super) contracts: HashMap<FnKey, Option<call_contracts::FuncContract>>,
    pub(super) class_methods: HashMap<String, std::collections::HashSet<String>>,
    pub(super) protocols: std::collections::HashSet<String>,
}

impl ContractCollector<'_> {
    pub(super) fn stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::FunctionDef(def) => {
                let key = (self.class.clone(), def.name.to_string());
                if let Some(class) = &self.class {
                    let methods = self.class_methods.entry(class.clone()).or_default();
                    methods.insert(def.name.to_string());
                    // `self.process = ..` shadows a method of that name on the instance.
                    methods.extend(instance_attribute_stores(def));
                }
                let linter = self.linter;
                let contract = call_contracts::binding_of(def, self.class.is_some()).map(|bound| {
                    let (line, _) = linter.source_location(def.range().start());
                    call_contracts::derive(def, bound, line, &|name| {
                        linter.schemas.get(name).cloned()
                    })
                });
                match self.contracts.entry(key.clone()) {
                    std::collections::hash_map::Entry::Occupied(mut seen) => {
                        let same = seen
                            .get()
                            .as_ref()
                            .zip(contract.as_ref())
                            .is_some_and(|(a, b)| a.agrees_with(b));
                        if !same {
                            seen.insert(None);
                        }
                    }
                    std::collections::hash_map::Entry::Vacant(slot) => {
                        slot.insert(contract);
                    }
                }
            }
            Stmt::ClassDef(class_def) => {
                self.stmt_class_def(class_def);
            }
            _ => {}
        }
    }

    fn stmt_class_def(&mut self, class_def: &ruff_python_ast::StmtClassDef) {
        let name = class_def.name.to_string();
        let is_protocol = class_def.bases().iter().any(|base| {
            let base = match base {
                Expr::Subscript(sub) => &*sub.value,
                other => other,
            };
            matches!(base, Expr::Name(n) if n.id.as_str() == "Protocol")
                || matches!(base, Expr::Attribute(a) if a.attr.as_str() == "Protocol")
        });
        if is_protocol {
            self.protocols.insert(name.clone());
        }
        self.class_methods.entry(name.clone()).or_default();
        let outer = self.class.replace(name.clone());
        for member in &class_def.body {
            // `name = other_function` in a class body also defines a method.
            if let Stmt::Assign(assign) = member {
                let aliases = assign.targets.iter().filter_map(|target| match target {
                    Expr::Name(t) => Some(t.id.to_string()),
                    _ => None,
                });
                self.class_methods
                    .entry(name.clone())
                    .or_default()
                    .extend(aliases);
            }
            self.stmt(member);
        }
        self.class = outer;
    }
}

// Attributes a method stores on its first parameter (`self.x = ..`, `setattr(self, "x", ..)`),
// nested functions included.
fn instance_attribute_stores(def: &ast::StmtFunctionDef) -> Vec<String> {
    let Some(first) = def
        .parameters
        .posonlyargs
        .iter()
        .chain(&def.parameters.args)
        .next()
    else {
        return Vec::new();
    };
    let mut stores = Stores {
        receiver: first.parameter.name.as_str(),
        found: Vec::new(),
    };
    for stmt in &def.body {
        stores.visit_stmt(stmt);
    }
    stores.found
}

struct Stores<'n> {
    pub(super) receiver: &'n str,
    pub(super) found: Vec<String>,
}

impl<'a> Visitor<'a> for Stores<'_> {
    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Attribute(a)
                if a.ctx == ast::ExprContext::Store
                    && matches!(&*a.value, Expr::Name(n) if n.id.as_str() == self.receiver) =>
            {
                self.found.push(a.attr.to_string());
            }
            Expr::Call(call) if matches!(&*call.func, Expr::Name(n) if n.id.as_str() == "setattr") => {
                if let [Expr::Name(n), Expr::StringLiteral(s), ..] = call.arguments.args.as_ref() {
                    if n.id.as_str() == self.receiver {
                        self.found.push(s.value.to_str().to_string());
                    }
                }
            }
            _ => {}
        }
        ast_visitor::walk_expr(self, expr);
    }
}

// Callee position -> contract key, for each call whose name binds to a single module-level
// function of the file, a `from` import, or a function of a plainly imported module.
struct ContractTargets<'a, 'k> {
    pub(super) analysis: &'k static_eval::Analysis<'a>,
    pub(super) module_functions: std::collections::HashSet<u32>,
    pub(super) out: HashMap<u32, FnKey>,
}

// See `ContractTargets`. Built from the module as written: folding keeps every source range.
pub(crate) fn contract_targets(
    analysis: &static_eval::Analysis,
    module: &ModModule,
) -> HashMap<u32, FnKey> {
    let mut targets = ContractTargets {
        analysis,
        module_functions: module
            .body
            .iter()
            .filter_map(|stmt| match stmt {
                Stmt::FunctionDef(def) => Some(def.range().start().to_u32()),
                _ => None,
            })
            .collect(),
        out: HashMap::new(),
    };
    for stmt in &module.body {
        targets.visit_stmt(stmt);
    }
    targets.out
}

impl<'v> Visitor<'v> for ContractTargets<'_, '_> {
    fn visit_expr(&mut self, expr: &'v Expr) {
        if let Expr::Call(call) = expr {
            self.record_call_target(call);
        }
        ast_visitor::walk_expr(self, expr);
    }
}

impl<'a, 'k> ContractTargets<'a, 'k> {
    fn record_call_target(&mut self, call: &ruff_python_ast::ExprCall) {
        let at = call.func.range().start().to_u32();
        match &*call.func {
            Expr::Name(name) => match self.analysis.callee_binding(name) {
                Some(static_eval::CalleeBinding::Function(def))
                    if self
                        .module_functions
                        .contains(&def.range().start().to_u32()) =>
                {
                    self.out.insert(at, (None, def.name.to_string()));
                }
                Some(static_eval::CalleeBinding::Function(_)) => {}
                Some(static_eval::CalleeBinding::Imported { local }) => {
                    self.out
                        .insert(at, (Some(call_contracts::IMPORTED_SLOT.to_string()), local));
                }
                // Only a `*` import's contracts are stored under a name the file never binds.
                None if self.analysis.binds_nowhere(name) => {
                    self.out.insert(
                        at,
                        (
                            Some(call_contracts::IMPORTED_SLOT.to_string()),
                            name.id.to_string(),
                        ),
                    );
                }
                None => {}
            },
            Expr::Attribute(attr) => {
                if let Expr::Name(base) = &*attr.value {
                    if matches!(
                        self.analysis.import_kind(base),
                        Some(static_eval::ImportKind::Module)
                    ) {
                        self.out.insert(
                            at,
                            (Some(format!("module:{}", base.id)), attr.attr.to_string()),
                        );
                    }
                }
            }
            _ => {}
        }
    }
}

// AST visitor collecting the name of every function and method defined anywhere in the
// module -- see `Linter::file_function_names`.
pub(super) struct FunctionNameCollector {
    pub(super) names: std::collections::HashSet<String>,
    pub(super) summaries: HashMap<FnKey, HelperSummary>,
    pub(super) seen: std::collections::HashSet<FnKey>,
    pub(super) current_class: Option<String>,
    pub(super) bases: HashMap<String, Vec<String>>,
    // Return annotations of each class's methods -- the part of `class_methods` that needs
    // no body inference, so it can be known before the walk reaches the class.
    pub(super) annotated_methods: HashMap<String, HashMap<String, String>>,
}

impl<'a> Visitor<'a> for FunctionNameCollector {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::ClassDef(class_def) => {
                let bases = class_def
                    .bases()
                    .iter()
                    .filter_map(|base| match base {
                        Expr::Name(name) => Some(name.id.to_string()),
                        Expr::Attribute(attr) => Some(attr.attr.to_string()),
                        _ => None,
                    })
                    .collect();
                self.bases.insert(class_def.name.to_string(), bases);
                self.annotated_methods
                    .entry(class_def.name.to_string())
                    .or_default();
                let outer = self.current_class.replace(class_def.name.to_string());
                ast_visitor::walk_stmt(self, stmt);
                self.current_class = outer;
                return;
            }
            Stmt::FunctionDef(func_def) => {
                let key = (self.current_class.clone(), func_def.name.to_string());
                self.names.insert(func_def.name.to_string());
                if let (Some(class), Some(returns)) = (&self.current_class, &func_def.returns) {
                    let annotated = if let Some(schema) =
                        ast_extract::extract_schema_from_annotation(returns)
                    {
                        Some(schema.to_string())
                    } else {
                        ast_extract::extract_bare_dataframe_type(returns)
                            .then(|| OPEN_FRAME_MARKER.to_string())
                    };
                    if let Some(annotated) = annotated {
                        self.annotated_methods
                            .entry(class.clone())
                            .or_default()
                            .insert(func_def.name.to_string(), annotated);
                    }
                }
                // A decorator can change what a call actually does or returns in ways this
                // checker has no way to see (memoization returning a stale/shared object,
                // a wrapper substituting its own return value, a context manager, ...).
                // `staticmethod`/`classmethod`/`abstractmethod`/`override` are transparent
                // -- they don't touch the call at runtime -- so a function bearing only
                // those still gets a summary; anything else forfeits one entirely, same as
                // a function this scan can't otherwise characterize.
                let opaque_decorator = func_def.decorator_list.iter().any(|decorator| {
                    !matches!(
                        ast_extract::decorator_name(&decorator.expression),
                        Some("staticmethod" | "classmethod" | "abstractmethod" | "override")
                    )
                });
                // A second definition of the same key (overload, conditional
                // redefinition) is only trusted if it agrees.
                let summary = (!opaque_decorator)
                    .then(|| summarize_helper(func_def))
                    .flatten();
                if !self.seen.insert(key.clone()) {
                    if self.summaries.get(&key) != summary.as_ref() {
                        self.summaries.remove(&key);
                    }
                } else if let Some(summary) = summary {
                    self.summaries.insert(key, summary);
                }
                // Functions nested in a method are not methods of the class.
                let outer = self.current_class.take();
                ast_visitor::walk_stmt(self, stmt);
                self.current_class = outer;
                return;
            }
            _ => {}
        }
        ast_visitor::walk_stmt(self, stmt);
    }
}
