use super::*;

impl NameResolver for Analysis<'_> {
    fn resolve(&self, name: &ast::ExprName) -> Resolved {
        match self.occurrence_group.get(&name.range().start()) {
            Some(Some(key)) => self
                .group_value(key)
                .map_or(Resolved::Unknown, Resolved::Value),
            Some(None) => Resolved::Builtin,
            None => Resolved::Unknown,
        }
    }

    fn resolve_attribute(&self, attribute: &ast::ExprAttribute) -> Resolved {
        if let Some(value) = self.module_class_attribute(attribute) {
            return Resolved::Value(value);
        }
        match self.attribute_group.get(&attribute.range()) {
            Some(Some(key)) => {
                return self
                    .group_value(key)
                    .map_or(Resolved::Unknown, Resolved::Value)
            }
            Some(None) => {}
            None => return Resolved::Unknown,
        }
        if let Some(value) = self.imported_class_attribute(attribute) {
            return value;
        }
        let Some(member) = self.imported_member(attribute) else {
            return Resolved::Unknown;
        };
        let Some(value) = member.constants.get(attribute.attr.as_str()) else {
            return Resolved::Unknown;
        };
        // Same rule as for any constant: a container this file aliases, passes on or
        // hands out the elements of must not be treated as fixed.
        let (interior, escapes) = self
            .module_attribute
            .get(&attribute.range())
            .and_then(|m| self.module_member_use.get(m))
            .copied()
            .unwrap_or((true, true));
        if (escapes && !value.is_immutable()) || (interior && !value.has_immutable_elements()) {
            return Resolved::Unknown;
        }
        Resolved::Value(value.clone())
    }

    fn is_module_member(&self, attribute: &ast::ExprAttribute) -> bool {
        self.imported_member(attribute)
            .is_some_and(|m| m.functions.contains_key(attribute.attr.as_str()))
    }

    fn call_module_function(
        &self,
        attribute: &ast::ExprAttribute,
        args: Vec<Value>,
        kwargs: Vec<(String, Value)>,
    ) -> Option<Value> {
        let module = self.imported_member(attribute)?;
        self.call_foreign(module, attribute.attr.as_str(), args, kwargs)
    }

    fn call_function(
        &self,
        callee: &ast::ExprName,
        args: Vec<Value>,
        kwargs: Vec<(String, Value)>,
    ) -> Option<Value> {
        let key = (*self.occurrence_group.get(&callee.range().start())?)?;
        let group = self.groups.get(&key)?;
        let id = std::ptr::from_ref(group) as usize;
        if let Some(ImportRef::From(level, module, name)) = &group.import {
            if group.bindings != 1 || (key.0 == 0 && self.dynamic_namespace) {
                return None;
            }
            let exports = self.imports.get(&(*level, module.clone()))?;
            return self.call_foreign(exports, name, args, kwargs);
        }
        let def = group.function?;
        if group.bindings != 1
            || (key.0 == 0 && self.dynamic_namespace)
            || def.is_async
            || !def.decorator_list.is_empty()
            || def.parameters.vararg.is_some()
            || def.parameters.kwarg.is_some()
        {
            return None;
        }
        // No recursion, and a ceiling on the work one file can ask for.
        {
            let stack = self.call_stack.borrow();
            if stack.contains(&id) || stack.len() >= MAX_CALL_DEPTH {
                return None;
            }
        }
        let calls = self.calls.get() + 1;
        if calls > MAX_CALLS {
            return None;
        }
        self.calls.set(calls);

        let bound = bind_arguments(&def.parameters, args, kwargs, |default| {
            Evaluator::new(self).eval(default)
        })?;

        self.call_stack.borrow_mut().push(id);
        let result = Evaluator::new(self).run_body(bound, &def.body);
        self.call_stack.borrow_mut().pop();
        result
    }
}

/// What a bare name in call position refers to, when exactly one thing can.
pub(crate) enum CalleeBinding<'a> {
    /// A function defined in this file.
    Function(&'a ast::StmtFunctionDef),
    /// A name brought in by `from module import name [as local]`.
    Imported { local: String },
}

impl<'a> Analysis<'a> {
    /// Whether this occurrence of a name refers to nothing the file binds anywhere, so it can
    /// only be a builtin.
    pub(crate) fn is_unbound_name(&self, name: &ast::ExprName) -> bool {
        matches!(self.resolve(name), Resolved::Builtin)
    }

    /// Whether this occurrence of a name is bound nowhere in the file: a builtin, or a name a
    /// `*` import brought in.
    pub(crate) fn binds_nowhere(&self, name: &ast::ExprName) -> bool {
        matches!(self.occurrence_group.get(&name.range().start()), Some(None))
    }

    /// The one plain, undecorated function a module-level `name` is bound to.
    pub(crate) fn single_module_function(&self, name: &str) -> Option<&'a ast::StmtFunctionDef> {
        if self.dynamic_namespace {
            return None;
        }
        let group = self.groups.get(&(0, name))?;
        let def = group.function?;
        (group.bindings == 1 && is_plain_function(def) && def.decorator_list.is_empty())
            .then_some(def)
    }

    /// How this name was imported, when it is bound once, by an import.
    pub(crate) fn import_kind(&self, name: &ast::ExprName) -> Option<ImportKind> {
        let key = self.occurrence_group.get(&name.range().start())?.as_ref()?;
        let group = self.groups.get(key)?;
        if group.bindings != 1 {
            return None;
        }
        match group.import {
            Some(ImportRef::Module(..)) => Some(ImportKind::Module),
            Some(ImportRef::From(..)) => Some(ImportKind::From),
            None => None,
        }
    }

    /// The single function or import the name `callee` refers to at this occurrence: bound
    /// once in its scope, in a namespace nothing can rewrite behind the bindings' backs, and
    /// (for a function) undecorated and not `async`.
    pub(crate) fn callee_binding(&self, callee: &ast::ExprName) -> Option<CalleeBinding<'a>> {
        let key = self
            .occurrence_group
            .get(&callee.range().start())?
            .as_ref()?;
        let group = self.groups.get(key)?;
        if group.bindings != 1 || (key.0 == 0 && self.dynamic_namespace) {
            return None;
        }
        if let Some(def) = group.function {
            return (is_plain_function(def) && def.decorator_list.is_empty())
                .then_some(CalleeBinding::Function(def));
        }
        matches!(group.import, Some(ImportRef::From(..))).then(|| CalleeBinding::Imported {
            local: key.1.to_string(),
        })
    }
}

impl Analysis<'_> {
    // `Col.HR` with `from cols import Col`: the attribute of the class that module exports,
    // under the same rule as any constant -- a container this file aliases, passes on or hands
    // out the elements of is not fixed.
    fn imported_class_attribute(&self, attribute: &ast::ExprAttribute) -> Option<Resolved> {
        let (key, attr) = self.imported_class_member.get(&attribute.range())?;
        if key.0 == 0 && self.dynamic_namespace {
            return Some(Resolved::Unknown);
        }
        let ImportRef::From(level, module, name) = self.groups.get(key)?.import.as_ref()? else {
            return None;
        };
        let exports = self.imports.get(&(*level, module.clone()))?;
        let value = exports.constants.get(&format!("{name}.{attr}"))?;
        let (interior, escapes) = self
            .module_member_use
            .get(&(*key, *attr))
            .copied()
            .unwrap_or((true, true));
        if (escapes && !value.is_immutable()) || (interior && !value.has_immutable_elements()) {
            return Some(Resolved::Unknown);
        }
        Some(Resolved::Value(value.clone()))
    }

    // The exports of the project module a `cfg.NAME` reaches through `import cfg`.
    pub(super) fn imported_member(
        &self,
        attribute: &ast::ExprAttribute,
    ) -> Option<&Arc<ModuleExports>> {
        let (key, _) = self.module_attribute.get(&attribute.range())?;
        if key.0 == 0 && self.dynamic_namespace {
            return None;
        }
        let ImportRef::Module(level, module) = self.groups.get(key)?.import.as_ref()? else {
            return None;
        };
        self.imports.get(&(*level, module.clone()))
    }

    // `cols.Col.A` through `import pkg.cols as cols`: the class attribute that module exports.
    // Only an immutable value, since this file's uses of it are not tracked.
    fn module_class_attribute(&self, attribute: &ast::ExprAttribute) -> Option<Value> {
        let Expr::Attribute(inner) = &*attribute.value else {
            return None;
        };
        let exports = self.imported_member(inner)?;
        let key = format!("{}.{}", inner.attr, attribute.attr);
        exports
            .constants
            .get(&key)
            .filter(|v| v.is_immutable())
            .cloned()
    }

    fn call_foreign(
        &self,
        exports: &ModuleExports,
        name: &str,
        args: Vec<Value>,
        kwargs: Vec<(String, Value)>,
    ) -> Option<Value> {
        let calls = self.calls.get() + 1;
        if calls > MAX_CALLS {
            return None;
        }
        self.calls.set(calls);
        let run = ForeignRun {
            exports,
            stack: RefCell::new(Vec::new()),
            calls: Cell::new(0),
        };
        run.call(name, args, kwargs)
    }
}

// Binds a call's arguments to a function's parameters. A default is evaluated by
// `default`, in the scope the function was defined in, not where it is called.
pub(super) fn bind_arguments(
    parameters: &ast::Parameters,
    args: Vec<Value>,
    kwargs: Vec<(String, Value)>,
    default: impl Fn(&Expr) -> Option<Value>,
) -> Option<HashMap<String, Value>> {
    let positional: Vec<&ast::ParameterWithDefault> = parameters
        .posonlyargs
        .iter()
        .chain(parameters.args.iter())
        .collect();
    if args.len() > positional.len() {
        return None;
    }
    let mut bound: HashMap<String, Value> = HashMap::new();
    for (param, value) in positional.iter().zip(args) {
        bound.insert(param.parameter.name.to_string(), value);
    }
    for (name, value) in kwargs {
        let known = parameters
            .args
            .iter()
            .chain(parameters.kwonlyargs.iter())
            .any(|p| p.parameter.name.as_str() == name);
        if !known || bound.contains_key(&name) {
            return None;
        }
        bound.insert(name, value);
    }
    for param in positional
        .iter()
        .copied()
        .chain(parameters.kwonlyargs.iter())
    {
        let name = param.parameter.name.as_str();
        if !bound.contains_key(name) {
            bound.insert(name.to_string(), default(param.default.as_deref()?)?);
        }
    }
    Some(bound)
}
