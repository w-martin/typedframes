use super::*;

impl Linter {
    // Checks each frame a call passes against what the callee requires of that parameter.
    // Silent when the callee can't be tied to one function or the arguments to its parameters.
    pub(super) fn check_call_requirements(
        &self,
        call: &ast::ExprCall,
        line: usize,
        col: usize,
        errors: &mut Vec<LintError>,
    ) {
        let Some((contract, origin)) = self.call_contract(call) else {
            return;
        };
        if !contract.has_requirements() {
            return;
        }
        let Some(shape) = call_contracts::CallShape::of(call) else {
            return;
        };
        let Some(bound) = contract.signature.bind(&shape) else {
            return;
        };
        if let ControlFlow::Break(_) = self.already_checked_call(call) {
            return;
        }
        let arguments = call
            .arguments
            .args
            .iter()
            .chain(call.arguments.keywords.iter().map(|k| &k.value));
        for (argument, param) in arguments.zip(bound) {
            let Some(param) = param else {
                continue;
            };
            let required = &contract.requires[param];
            if required.is_empty() {
                continue;
            }
            let Some((available, index_levels, arg_label)) = self.argument_columns(argument) else {
                continue;
            };
            let missing: Vec<&String> = required
                .iter()
                .filter(|c| !available.contains(c) && !index_levels.contains(c))
                .collect();
            let (missing_str, as_param) = match missing_columns_error(contract, param, missing) {
                Some(value) => value,
                None => continue,
            };
            errors.push(LintError {
                line,
                col,
                code: CODE_MISSING_COLUMN.to_string(),
                message: format!(
                    "'{}' passed to {}{} is missing column(s) {{{}}} — available: {{{}}}, required: {{{}}}",
                    arg_label,
                    origin,
                    as_param,
                    missing_str,
                    available.join(", "),
                    required.join(", ")
                ),
                severity: "error".to_string(),
            });
        }
    }

    // The columns a call argument is known to have, its index levels, and how to name it in a
    // message; `None` when they are not known.
    fn argument_columns(&self, argument: &Expr) -> Option<(Vec<String>, Vec<String>, String)> {
        match argument {
            Expr::Name(arg_name) => {
                let (schema_name, _) = self.variables.get(arg_name.id.as_str())?;
                // An unresolved frame's empty column list means "unknown", not "has no columns".
                if self.schema_is_unresolved(schema_name) {
                    return None;
                }
                let available = self.schemas.get(schema_name)?;
                Some((
                    available.clone(),
                    self.schema_index
                        .get(schema_name)
                        .cloned()
                        .unwrap_or_default(),
                    arg_name.id.to_string(),
                ))
            }
            // A load or chain written in place: `f(pl.read_parquet(.., columns=[..]))`.
            Expr::Call(_) => {
                let (available, _) = self.expr_frame_columns(argument)?;
                Some((available, Vec::new(), "the frame".to_string()))
            }
            _ => None,
        }
    }

    fn already_checked_call(&self, call: &ruff_python_ast::ExprCall) -> ControlFlow<()> {
        if !self
            .contract_calls_checked
            .borrow_mut()
            .insert((self.file_display.clone(), call.range().start().to_u32()))
        {
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }

    // The contract of the function `call` reaches, when that is one function.
    fn call_contract(
        &self,
        call: &ast::ExprCall,
    ) -> Option<(&call_contracts::FuncContract, &String)> {
        let found = match &*call.func {
            Expr::Name(name) => {
                if self.variables.contains_key(name.id.as_str()) {
                    return None;
                }
                let key = self
                    .contract_targets
                    .get(&call.func.range().start().to_u32())?;
                self.contracts.get(key)
            }
            Expr::Attribute(attr) => match &*attr.value {
                Expr::Name(base) if base.id.as_str() == "self" => {
                    let class = self.current_class_name.as_ref()?;
                    let key = self.method_contract_key(class, attr.attr.as_str())?;
                    self.contracts.get(&key)
                }
                Expr::Name(base) => {
                    if self.variables.contains_key(base.id.as_str()) {
                        return None;
                    }
                    if let Some(key) = self
                        .contract_targets
                        .get(&call.func.range().start().to_u32())
                    {
                        self.contracts.get(key)
                    } else {
                        let class = self.receiver_classes.get(base.id.as_str())?;
                        self.receiver_method_contract(class, attr.attr.as_str())
                    }
                }
                // `Loader().load(frame)`
                Expr::Call(inner) => {
                    let Expr::Name(class) = &*inner.func else {
                        return None;
                    };
                    let key = self.method_contract_key(class.id.as_str(), attr.attr.as_str())?;
                    self.contracts.get(&key)
                }
                _ => None,
            },
            _ => None,
        }?;
        Some((&found.0, &found.1))
    }

    // A call through a name annotated with (or constructed from) `class`. A protocol's own
    // bodies never run: its method means whatever every implementor in the project agrees on.
    fn receiver_method_contract(
        &self,
        class: &str,
        method: &str,
    ) -> Option<&(call_contracts::FuncContract, String)> {
        if self.protocol_classes.contains(class) {
            return self.method_agreement.get(method);
        }
        let key = self.method_contract_key(class, method)?;
        self.contracts.get(&key)
    }

    // The contract key of the method `self.<method>` reaches in an instance of `class`: the
    // nearest class in its chain that defines it, when every class before that one is known
    // not to, and no subclass in this file overrides it with a different requirement.
    fn method_contract_key(&self, class: &str, method: &str) -> Option<FnKey> {
        let memo_key = (Some(class.to_string()), method.to_string());
        if let Some(done) = self.method_contract_memo.borrow().get(&memo_key) {
            return done.clone();
        }
        let found = self.compute_method_contract_key(class, method);
        self.method_contract_memo
            .borrow_mut()
            .insert(memo_key, found.clone());
        found
    }

    fn compute_method_contract_key(&self, class: &str, method: &str) -> Option<FnKey> {
        if self.protocol_classes.contains(class) {
            return None;
        }
        let mut owner = None;
        for ancestor in self.class_chain(class) {
            if self.protocol_classes.contains(&ancestor) {
                return None;
            }
            let key = (Some(ancestor.clone()), method.to_string());
            match self.class_method_names.get(&ancestor) {
                Some(methods) if methods.contains(method) => {
                    owner = Some(key);
                    break;
                }
                Some(_) => continue,
                // A class from another file: only its own method contracts are known.
                None if self.contracts.contains_key(&key) => {
                    owner = Some(key);
                    break;
                }
                None => return None,
            }
        }
        let owner = owner?;
        // An instance of a subclass in another file would run that file's method instead.
        if self
            .overridden_elsewhere
            .get(class)
            .is_some_and(|methods| methods.contains(method))
        {
            return None;
        }
        let found = &self.contracts.get(&owner)?.0;
        for other in self.subclasses.get(class).into_iter().flatten() {
            let defines = self
                .class_method_names
                .get(other)
                .is_some_and(|methods| methods.contains(method));
            if !defines || Some(other) == owner.0.as_ref() {
                continue;
            }
            let agrees = self
                .contracts
                .get(&(Some(other.clone()), method.to_string()))
                .is_some_and(|(c, _)| c.agrees_with(found));
            if !agrees {
                return None;
            }
        }
        Some(owner)
    }

    // Collects the contract of every function and method of the file, resolves the calls
    // whose callee binds to exactly one of them, and folds in one level of forwarding.
    pub(super) fn prepare_contracts(&mut self, module: &ModModule) {
        self.contracts.clear();
        self.local_contracts.clear();
        self.class_method_names.clear();
        self.protocol_classes.clear();
        self.method_contract_memo.borrow_mut().clear();
        let mut collector = ContractCollector {
            linter: self,
            class: None,
            contracts: HashMap::new(),
            class_methods: HashMap::new(),
            protocols: std::collections::HashSet::new(),
        };
        for stmt in &module.body {
            collector.stmt(stmt);
        }
        let ContractCollector {
            contracts: collected,
            class_methods,
            protocols,
            ..
        } = collector;
        self.class_method_names = class_methods;
        self.protocol_classes = protocols;
        self.subclasses.clear();
        for class in self.class_method_names.keys() {
            for ancestor in self.class_chain(class).into_iter().skip(1) {
                self.subclasses
                    .entry(ancestor)
                    .or_default()
                    .push(class.clone());
            }
        }
        let mut local: HashMap<FnKey, call_contracts::FuncContract> = collected
            .into_iter()
            .filter_map(|(key, contract)| Some((key, contract?)))
            .collect();

        if !std::mem::take(&mut self.contract_targets_ready) {
            let analysis = static_eval::Analysis::build(module, &self.imported_modules);
            self.contract_targets = contract_targets(&analysis, module);
        }

        for (key, contract) in &local {
            let origin = format!("{} ({}:{})", key.1, self.file_display, contract.def_line);
            self.contracts
                .insert(key.clone(), (contract.clone(), origin));
        }
        for (key, entry) in &self.imported_contracts {
            self.contracts
                .entry(key.clone())
                .or_insert_with(|| entry.clone());
        }
        // One level: a forwarded parameter takes on what the callee itself requires.
        let mut pending: Vec<(FnKey, call_contracts::FuncContract)> = Vec::new();
        for (key, contract) in local.iter_mut() {
            self.finish_local_contract(&mut pending, key, contract);
        }
        for (key, contract) in pending {
            if let Some((existing, _)) = self.contracts.get_mut(&key) {
                existing.requires = contract.requires.clone();
            }
            self.local_contracts.insert(key, contract);
        }
        self.method_contract_memo.borrow_mut().clear();
    }

    fn finish_local_contract(
        &mut self,
        pending: &mut Vec<((Option<String>, String), call_contracts::FuncContract)>,
        key: &(Option<String>, String),
        contract: &mut call_contracts::FuncContract,
    ) {
        let finished = self.resolve_local_forwards(key, contract);
        pending.push((key.clone(), finished));
    }

    fn resolve_local_forwards(
        &mut self,
        key: &(Option<String>, String),
        contract: &mut call_contracts::FuncContract,
    ) -> call_contracts::FuncContract {
        let forwards = std::mem::take(&mut contract.forwards);
        let mut unresolved = Vec::new();
        for mut forward in forwards {
            let target = match &forward.target {
                call_contracts::ForwardTarget::Call(at) => self.contract_targets.get(at).cloned(),
                call_contracts::ForwardTarget::SelfMethod(method) => key
                    .0
                    .as_ref()
                    .and_then(|class| self.method_contract_key(class, method)),
                _ => None,
            };
            match target.as_ref().and_then(|t| self.contracts.get(t)) {
                Some((callee, _)) => contract.inherit(&forward, callee),
                None => {
                    // Left for the project index: a function imported from another file.
                    let later = match target {
                        Some((Some(slot), name)) if slot == call_contracts::IMPORTED_SLOT => {
                            Some(call_contracts::ForwardTarget::Imported(name))
                        }
                        Some((Some(slot), name)) => slot.strip_prefix("module:").map(|alias| {
                            call_contracts::ForwardTarget::Module(alias.to_string(), name)
                        }),
                        _ => None,
                    };
                    if let Some(later) = later {
                        forward.target = later;
                        unresolved.push(forward);
                    }
                }
            }
        }
        let mut finished = contract.clone();
        finished.forwards = unresolved;
        finished
    }
}

fn missing_columns_error(
    contract: &call_contracts::FuncContract,
    param: usize,
    missing: Vec<&String>,
) -> Option<(String, String)> {
    if missing.is_empty() {
        return None;
    }
    let missing_str = missing
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let as_param = if Some(param) == contract.loose_param || param == 0 {
        String::new()
    } else {
        format!(" as '{}'", contract.signature.name(param))
    };
    Some((missing_str, as_param))
}
