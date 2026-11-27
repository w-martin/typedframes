use super::*;

impl Linter {
    pub(super) fn visit_class_def(
        &mut self,
        errors: &mut Vec<LintError>,
        class_def: &ruff_python_ast::StmtClassDef,
    ) {
        self.register_schema_class(class_def, errors);
        self.register_record_class(class_def);

        // Pre-scan this class's own __init__ for self.<attr> origins (see
        // current_self_attrs' doc comment) BEFORE visiting the body, so calls
        // anywhere in the class -- including __init__ itself -- can resolve
        // self.<attr>.<method>(...). Saved/restored around visit_body so a
        // nested class definition (rare, but not impossible) can't leak its
        // own attrs into, or clobber, an enclosing class's.
        let mut self_attrs = HashMap::new();
        for body_stmt in &class_def.body {
            if let Stmt::FunctionDef(func_def) = body_stmt {
                if func_def.name.as_str() == "__init__" {
                    collect_self_attr_origins(
                        &func_def.body,
                        &annotated_param_origins(func_def),
                        &mut self_attrs,
                    );
                }
            }
        }
        self.all_self_attr_origins.extend(self_attrs.clone());
        let prev_self_attrs = std::mem::replace(&mut self.current_self_attrs, self_attrs);
        let own_method_names = class_def
            .body
            .iter()
            .filter_map(|body_stmt| match body_stmt {
                Stmt::FunctionDef(func_def) => Some(func_def.name.to_string()),
                _ => None,
            })
            .collect();
        let prev_class_methods =
            std::mem::replace(&mut self.current_class_methods, own_method_names);
        let own_methods =
            self.visit_class_body(errors, class_def, prev_self_attrs, prev_class_methods);

        // Record this class's own methods' return schemas into class_methods
        // (see its doc comment) -- accumulated, never reset, so a DIFFERENT
        // class visited later in the file can resolve a self.<attr> typed as
        // THIS class. A method with no explicit return annotation resolves here
        // exactly the way a module-level function does (via body inference).
        self.class_methods
            .insert(class_def.name.to_string(), own_methods);
    }

    fn visit_class_body(
        &mut self,
        errors: &mut Vec<LintError>,
        class_def: &ruff_python_ast::StmtClassDef,
        prev_self_attrs: HashMap<String, SelfAttrOrigin>,
        prev_class_methods: std::collections::HashSet<String>,
    ) -> HashMap<String, String> {
        let prev_class_functions = std::mem::take(&mut self.current_class_functions);
        let prev_class_name = self.current_class_name.replace(class_def.name.to_string());

        // Visit the class body regardless of which branch above matched (or
        // none did). A schema/ORM class's body is a declarative column list
        // that the branches above already extracted -- re-visiting it as
        // ordinary statements is a no-op for those (Column(...)-style
        // AnnAssigns don't match any DataFrame/Annotated annotation shape).
        // For every other class -- which is most business-logic code, the
        // shape Protocol implementations and DI-style service classes take --
        // this is the ONLY way its methods are ever seen: without it, a
        // `pd.read_csv(...)` or a `self, df` method body sitting inside a
        // plain `class Loader:` is invisible to the entire rest of this
        // checker, the same way it would be if the file were never linted at
        // all.
        //
        // Each method is visited with any same-named entry in the flat
        // `functions` map set aside, so a module-level function or another
        // class's method of that name cannot block this method's own return
        // inference or be mistaken for it; the result is also recorded per class
        // in `current_class_functions`, which `self.<method>()` reads.
        //
        // Names bound directly in the class body live in the class's own
        // scope: methods do not see them, and they do not outlive the class.
        let mut module_variables = self.variables.clone();
        for body_stmt in &class_def.body {
            self.note_class_member(errors, &mut module_variables, body_stmt);
        }

        self.variables = module_variables;
        self.current_self_attrs = prev_self_attrs;
        self.current_class_methods = prev_class_methods;
        let own_methods =
            std::mem::replace(&mut self.current_class_functions, prev_class_functions);
        self.current_class_name = prev_class_name;
        own_methods
    }

    fn note_class_member(
        &mut self,
        errors: &mut Vec<LintError>,
        module_variables: &mut HashMap<String, (String, usize)>,
        body_stmt: &Stmt,
    ) {
        let method_name = match body_stmt {
            Stmt::FunctionDef(func_def) => Some(func_def.name.to_string()),
            _ => None,
        };
        let set_aside = method_name
            .as_deref()
            .and_then(|name| self.functions.remove(name));
        if let Stmt::FunctionDef(func_def) = body_stmt {
            let class_variables = std::mem::replace(&mut self.variables, module_variables.clone());
            self.visit_stmt(body_stmt, errors);
            let after_method = std::mem::replace(&mut self.variables, class_variables);
            // `global` writes made by the method belong to the module.
            for name in global_names(&func_def.body) {
                match after_method.get(&name) {
                    Some(binding) => module_variables.insert(name, binding.clone()),
                    None => module_variables.remove(&name),
                };
            }
        } else {
            self.visit_stmt(body_stmt, errors);
        }
        if let Some(name) = method_name {
            // The flat entry stays: attribute calls on an untracked receiver
            // (`source.load()` through a Protocol) resolve by method name.
            match self.functions.get(&name).cloned() {
                Some(schema_or_marker) => {
                    self.current_class_functions.insert(name, schema_or_marker);
                }
                None => {
                    if let Some(previous) = set_aside {
                        self.functions.insert(name, previous);
                    }
                }
            }
        }
    }
}
