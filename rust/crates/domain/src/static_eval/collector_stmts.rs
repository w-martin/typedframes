use super::*;

impl<'a> Collector<'a> {
    pub(super) fn stmt(&mut self, stmt: &'a ast::Stmt) {
        use ast::Stmt;
        match stmt {
            Stmt::FunctionDef(f) => self.stmt_function_def(f),
            Stmt::ClassDef(c) => self.stmt_class_def(c),
            Stmt::Return(r) => {
                if let Some(v) = &r.value {
                    self.expr(v, Use::Escapes);
                }
            }
            Stmt::Delete(d) => d.targets.iter().for_each(|t| self.target(t)),
            // `a, b = x` reads x's elements; any other target keeps a reference to x.
            Stmt::Assign(a) => self.stmt_assign(a),
            Stmt::AugAssign(a) => self.stmt_aug_assign(a),
            Stmt::AnnAssign(a) => self.stmt_ann_assign(a),
            Stmt::For(f) => self.stmt_for(f),
            Stmt::While(w) => self.stmt_while(w),
            Stmt::If(i) => self.stmt_if(i),
            Stmt::With(w) => self.stmt_with(w),
            Stmt::Match(m) => self.stmt_match(m),
            Stmt::Raise(r) => self.stmt_raise(r),
            Stmt::Try(t) => self.stmt_try(t),
            Stmt::Assert(a) => self.stmt_assert(a),
            Stmt::Import(i) => self.stmt_import(i),
            Stmt::ImportFrom(i) => self.stmt_import_from(i),
            Stmt::Global(g) => self.declare(&g.names, false),
            Stmt::Nonlocal(n) => self.declare(&n.names, true),
            Stmt::TypeAlias(t) => self.target(&t.name),
            Stmt::Expr(e) => self.expr(&e.value, Use::Safe),
            Stmt::Pass(_) | Stmt::Break(_) | Stmt::Continue(_) | Stmt::IpyEscapeCommand(_) => {}
        }
    }

    fn stmt_for(&mut self, f: &'a ast::StmtFor) {
        self.expr(&f.iter, Use::Interior);
        self.target(&f.target);
        self.body(&f.body);
        self.body(&f.orelse);
    }

    fn stmt_while(&mut self, w: &'a ast::StmtWhile) {
        self.expr(&w.test, Use::Safe);
        self.body(&w.body);
        self.body(&w.orelse);
    }

    fn stmt_raise(&mut self, r: &'a ast::StmtRaise) {
        r.exc
            .iter()
            .chain(r.cause.iter())
            .for_each(|e| self.expr(e, Use::Escapes));
    }

    fn stmt_assert(&mut self, a: &'a ast::StmtAssert) {
        self.expr(&a.test, Use::Safe);
        if let Some(msg) = &a.msg {
            self.expr(msg, Use::Safe);
        }
    }

    fn stmt_import(&mut self, i: &'a ast::StmtImport) {
        for alias in &i.names {
            let (bound, binding) = import_binding(alias);
            self.bind(bound, binding);
        }
    }

    // `global` or `nonlocal` names declared in the current scope.
    fn declare(&mut self, names: &'a [ast::Identifier], nonlocal: bool) {
        let scope = &mut self.scopes[self.current];
        for name in names {
            if nonlocal {
                scope.nonlocals.insert(name.as_str());
            } else {
                scope.globals.insert(name.as_str());
            }
        }
    }

    fn stmt_with(&mut self, w: &'a ruff_python_ast::StmtWith) {
        for item in &w.items {
            self.expr(&item.context_expr, Use::Escapes);
            if let Some(vars) = &item.optional_vars {
                self.target(vars);
            }
        }
        self.body(&w.body);
    }

    fn stmt_match(&mut self, m: &'a ruff_python_ast::StmtMatch) {
        self.expr(&m.subject, Use::Escapes);
        for case in &m.cases {
            self.pattern(&case.pattern);
            if let Some(guard) = &case.guard {
                self.expr(guard, Use::Safe);
            }
            self.body(&case.body);
        }
    }

    fn stmt_if(&mut self, i: &'a ruff_python_ast::StmtIf) {
        self.expr(&i.test, Use::Safe);
        self.body(&i.body);
        for clause in &i.elif_else_clauses {
            if let Some(test) = &clause.test {
                self.expr(test, Use::Safe);
            }
            self.body(&clause.body);
        }
    }

    fn stmt_aug_assign(&mut self, a: &'a ruff_python_ast::StmtAugAssign) {
        self.expr(&a.value, Use::Escapes);
        match &*a.target {
            Expr::Name(n) => {
                // `x += y` reads and rebinds `x`.
                self.occurrence_force(n, Use::Escapes);
                self.bind(n.id.as_str(), Binding::Other);
            }
            other => self.target(other),
        }
    }

    fn stmt_function_def(&mut self, f: &'a ruff_python_ast::StmtFunctionDef) {
        for d in &f.decorator_list {
            self.expr(&d.expression, Use::Escapes);
        }
        self.parameter_defaults(&f.parameters);
        self.bind(f.name.as_str(), Binding::Function(f));
        let outer = self.current;
        self.push_scope(ScopeKind::Function);
        self.parameters(&f.parameters);
        self.body(&f.body);
        self.current = outer;
    }

    fn stmt_try(&mut self, t: &'a ruff_python_ast::StmtTry) {
        self.body(&t.body);
        for handler in &t.handlers {
            let ast::ExceptHandler::ExceptHandler(h) = handler;
            if let Some(type_) = &h.type_ {
                self.expr(type_, Use::Safe);
            }
            if let Some(name) = &h.name {
                self.bind(name.as_str(), Binding::Other);
            }
            self.body(&h.body);
        }
        self.body(&t.orelse);
        self.body(&t.finalbody);
    }

    fn stmt_import_from(&mut self, i: &'a ruff_python_ast::StmtImportFrom) {
        for alias in &i.names {
            if alias.name.as_str() == "*" {
                self.dynamic_namespace = true;
            } else {
                let bound = alias.asname.as_ref().unwrap_or(&alias.name);
                let module = i.module.as_ref().map(|m| m.to_string());
                self.bind(
                    bound.as_str(),
                    Binding::ImportFrom(
                        i.level,
                        module.unwrap_or_default(),
                        alias.name.to_string(),
                    ),
                );
            }
        }
    }

    fn stmt_assign(&mut self, a: &'a ruff_python_ast::StmtAssign) {
        let unpacking = a
            .targets
            .iter()
            .all(|t| matches!(t, Expr::Tuple(_) | Expr::List(_)));
        self.expr(
            &a.value,
            if unpacking {
                Use::Interior
            } else {
                Use::Escapes
            },
        );
        match a.targets.as_slice() {
            [Expr::Name(n)] => self.bind(n.id.as_str(), Binding::Assign(&a.value)),
            targets => targets.iter().for_each(|t| self.target(t)),
        }
    }

    fn stmt_class_def(&mut self, c: &'a ruff_python_ast::StmtClassDef) {
        for d in &c.decorator_list {
            self.expr(&d.expression, Use::Escapes);
        }
        if let Some(arguments) = &c.arguments {
            arguments
                .args
                .iter()
                .for_each(|a| self.expr(a, Use::Escapes));
            arguments
                .keywords
                .iter()
                .for_each(|k| self.expr(&k.value, Use::Escapes));
        }
        let outer = self.current;
        let body_scope = self.push_scope(ScopeKind::Class);
        self.bind_in(outer, c.name.as_str(), Binding::Class(c, body_scope));
        self.body(&c.body);
        self.current = outer;
    }

    fn stmt_ann_assign(&mut self, a: &'a ruff_python_ast::StmtAnnAssign) {
        if let Some(v) = &a.value {
            self.expr(v, Use::Escapes);
        }
        if let (Expr::Name(n), Some(_)) = (&*a.target, &a.value) {
            if annotation_is_final(&a.annotation) {
                self.final_bindings.insert((self.current, n.id.as_str()));
            }
        }
        match (&*a.target, &a.value) {
            // An annotation can declare the name to be something its value isn't
            // (`df: Annotated[pl.DataFrame, S] = None` is a DataFrame to the
            // checker), so only a plain builtin type leaves the value as the truth.
            (Expr::Name(n), Some(v)) if annotation_is_plain(&a.annotation) => {
                self.bind(n.id.as_str(), Binding::Assign(v))
            }
            (Expr::Name(n), Some(_)) => self.bind(n.id.as_str(), Binding::Other),
            (Expr::Name(n), None) => self.bind(n.id.as_str(), Binding::Other),
            (other, _) => self.target(other),
        }
    }

    // A name that is read and rebound in one statement (`x += y`): ruff gives its `ctx`
    // as Store, but it is a use all the same.
    fn occurrence_force(&mut self, name: &'a ast::ExprName, use_: Use) {
        self.occurrences.push(Occurrence {
            offset: name.range().start(),
            scope: self.current,
            name: name.id.as_str(),
            use_,
            builtin: None,
        });
    }
}
