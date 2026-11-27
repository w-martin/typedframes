use super::*;

/// What one project module offers to the files that import it: its statically known
/// constants and the source of its evaluable functions. Computed when the project is
/// indexed, so an importing file never has to analyse the module again.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ModuleExports {
    pub(crate) constants: HashMap<String, Value>,
    pub(crate) functions: HashMap<String, String>,
    // String enums the module defines, by class name, with every name their body binds.
    #[serde(default)]
    pub(crate) enums: HashMap<String, Vec<String>>,
    // `from module import name [as local]` at module level, by local name: what an importer
    // of this module (a package `__init__.py` re-exporting, typically) reaches through it.
    #[serde(default)]
    pub(crate) reexports: HashMap<String, (u32, String, String)>,
    // Evaluable builtins the module rebinds, so one of its functions isn't evaluated as
    // if `len` were the builtin.
    pub(super) shadowed: Vec<String>,
}

impl ModuleExports {
    pub(crate) fn is_empty(&self) -> bool {
        self.constants.is_empty()
            && self.functions.is_empty()
            && self.enums.is_empty()
            && self.reexports.is_empty()
    }
}

/// Imported modules by `(leading dots, dotted name)` as written in the importing file.
pub(crate) type ImportedModules = HashMap<(u32, String), Arc<ModuleExports>>;

const MAX_EXPORTED_NODES: usize = 1_000;

// The builtins the evaluator implements; rebinding one anywhere changes what a call to
// it means.
pub(super) const EVALUATED_BUILTINS: &[&str] = &[
    "len",
    "list",
    "tuple",
    "set",
    "frozenset",
    "sorted",
    "reversed",
    "range",
    "zip",
    "enumerate",
    "min",
    "max",
    "sum",
    "dict",
    "frozendict",
    "str",
    "int",
    "bool",
    "any",
    "all",
];

/// The statically known constants and evaluable functions of `module`, for importers.
pub(crate) fn export_module(analysis: &Analysis, source: &str) -> ModuleExports {
    let mut exports = ModuleExports::default();
    if !analysis.complete || analysis.dynamic_namespace {
        return exports;
    }
    exports.shadowed = analysis.shadowed.clone();
    let mut candidates: HashMap<&str, &ast::StmtFunctionDef> = HashMap::new();
    for ((scope, name), group) in &analysis.groups {
        if *scope != 0 {
            continue;
        }
        if group.assign.is_some() {
            if let Some(value) = analysis.group_value(&(0, name)) {
                if node_count(&value) <= MAX_EXPORTED_NODES {
                    exports.constants.insert(name.to_string(), value);
                }
            }
        } else if let Some(def) = group.function {
            if group.bindings == 1 && is_plain_function(def) {
                candidates.insert(name, def);
            }
        } else if let Some(ImportRef::From(level, module, imported)) = &group.import {
            if group.bindings == 1 {
                exports
                    .reexports
                    .insert(name.to_string(), (*level, module.clone(), imported.clone()));
            }
        }
    }
    for (class, members) in &analysis.string_enums {
        exports.enums.insert(class.to_string(), members.clone());
    }
    for (class, attr, key) in &analysis.class_exports {
        if let Some(value) = analysis.group_value(key) {
            if node_count(&value) <= MAX_EXPORTED_NODES {
                exports.constants.insert(format!("{class}.{attr}"), value);
            }
        }
    }
    for (name, def) in &candidates {
        // A local that shares a name with anything the body could otherwise read from the
        // module (a constant, another function, a builtin) might be read before it is
        // assigned; the importing side resolves names without scopes, so refuse it.
        let clashes = assigned_names(&def.body).iter().any(|local| {
            exports.constants.contains_key(local)
                || candidates.contains_key(local.as_str())
                || EVALUATED_BUILTINS.contains(&local.as_str())
        });
        if clashes {
            continue;
        }
        let range = def.range();
        if let Some(text) = source.get(usize::from(range.start())..usize::from(range.end())) {
            exports.functions.insert(name.to_string(), text.to_string());
        }
    }
    exports
}

pub(super) fn is_plain_function(def: &ast::StmtFunctionDef) -> bool {
    !def.is_async
        && def.decorator_list.is_empty()
        && def.parameters.vararg.is_none()
        && def.parameters.kwarg.is_none()
}

fn assigned_names(body: &[ast::Stmt]) -> Vec<String> {
    use ast::Stmt;
    let mut names = Vec::new();
    for stmt in body {
        match stmt {
            Stmt::Assign(a) => {
                for target in &a.targets {
                    if let Expr::Name(n) = target {
                        names.push(n.id.to_string());
                    }
                }
            }
            Stmt::AnnAssign(a) => {
                if let Expr::Name(n) = &*a.target {
                    names.push(n.id.to_string());
                }
            }
            _ => {}
        }
    }
    names
}

// One evaluation of a function that lives in another module. Names resolve against that
// module's exports only; the importing file's own names are never visible to it.
pub(super) struct ForeignRun<'e> {
    pub(super) exports: &'e ModuleExports,
    pub(super) stack: RefCell<Vec<String>>,
    pub(super) calls: Cell<u32>,
}

impl ForeignRun<'_> {
    pub(super) fn call(
        &self,
        name: &str,
        args: Vec<Value>,
        kwargs: Vec<(String, Value)>,
    ) -> Option<Value> {
        {
            let stack = self.stack.borrow();
            if stack.iter().any(|n| n == name) || stack.len() >= MAX_CALL_DEPTH {
                return None;
            }
        }
        let calls = self.calls.get() + 1;
        if calls > MAX_CALLS {
            return None;
        }
        self.calls.set(calls);

        let text = self.exports.functions.get(name)?;
        let parsed = ruff_python_parser::parse_module(text).ok()?.into_syntax();
        let [ast::Stmt::FunctionDef(def)] = parsed.body.as_slice() else {
            return None;
        };
        let bound = bind_arguments(&def.parameters, args, kwargs, |default| {
            Evaluator::new(self).eval(default)
        })?;
        self.stack.borrow_mut().push(name.to_string());
        let result = Evaluator::new(self).run_body(bound, &def.body);
        self.stack.borrow_mut().pop();
        result
    }
}

impl NameResolver for ForeignRun<'_> {
    fn resolve(&self, name: &ast::ExprName) -> Resolved {
        let id = name.id.as_str();
        if let Some(value) = self.exports.constants.get(id) {
            Resolved::Value(value.clone())
        } else if self.exports.functions.contains_key(id)
            || self.exports.shadowed.iter().any(|s| s == id)
        {
            Resolved::Unknown
        } else {
            Resolved::Builtin
        }
    }

    fn call_function(
        &self,
        callee: &ast::ExprName,
        args: Vec<Value>,
        kwargs: Vec<(String, Value)>,
    ) -> Option<Value> {
        self.call(callee.id.as_str(), args, kwargs)
    }
}

// The group holding `base.attr` when `base` is a class defined in the file and `attr` is
// assigned in its body (or in its single user-defined base's). Refused for a class a
// decorator (other than `dataclasses.dataclass`), a metaclass, an outside base class (an
// enum, a `BaseSchema`, an ORM base), several bases, a rebinding of its name, or
// `setattr`/`delattr`/`vars` could have changed. `base` may also be a frozen dataclass
// instance built with no arguments, whose fields are the class's defaults.
pub(super) fn class_attribute<'a>(
    groups: &FastMap<GroupKey<'a>, Group<'a>>,
    scopes: &[Scope<'a>],
    dynamic: &FastSet<usize>,
    from: usize,
    base: &str,
    attr: &'a str,
) -> Option<GroupKey<'a>> {
    let mut key = lookup(scopes, from, base).filter(|k| groups.contains_key(k))?;
    if groups.get(&key)?.class.is_none() {
        key = frozen_dataclass_instance_class(groups, scopes, key)?;
    }
    class_attribute_of(groups, scopes, dynamic, key, attr)
}
