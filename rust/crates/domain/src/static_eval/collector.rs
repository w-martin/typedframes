use super::*;

impl<'a> Collector<'a> {
    pub(super) fn new() -> Self {
        Collector {
            scopes: vec![Scope {
                kind: ScopeKind::Module,
                parent: None,
                bindings: FastMap::default(),
                globals: FastSet::default(),
                nonlocals: FastSet::default(),
            }],
            current: 0,
            occurrences: Vec::new(),
            pending_nonlocal: Vec::new(),
            attribute_occurrences: Vec::new(),
            pending_attribute_stores: Vec::new(),
            final_bindings: HashSet::new(),
            dynamic_class_targets: Vec::new(),
            dynamic_namespace: false,
            builtin: None,
        }
    }

    pub(super) fn push_scope(&mut self, kind: ScopeKind) -> usize {
        self.scopes.push(Scope {
            kind,
            parent: Some(self.current),
            bindings: FastMap::default(),
            globals: FastSet::default(),
            nonlocals: FastSet::default(),
        });
        let id = self.scopes.len() - 1;
        self.current = id;
        id
    }

    pub(super) fn bind_in(&mut self, scope: usize, name: &'a str, binding: Binding<'a>) {
        if self.scopes[scope].globals.contains(name) {
            self.scopes[0]
                .bindings
                .entry(name)
                .or_default()
                .push(binding);
        } else if self.scopes[scope].nonlocals.contains(name) {
            self.pending_nonlocal.push((scope, name));
        } else {
            self.scopes[scope]
                .bindings
                .entry(name)
                .or_default()
                .push(binding);
        }
    }

    pub(super) fn bind(&mut self, name: &'a str, binding: Binding<'a>) {
        self.bind_in(self.current, name, binding);
    }

    // A walrus inside a comprehension binds in the enclosing function/module scope.
    pub(super) fn bind_walrus(&mut self, name: &'a str) {
        let mut scope = self.current;
        while self.scopes[scope].kind == ScopeKind::Comprehension {
            scope = self.scopes[scope].parent.unwrap_or(0);
        }
        self.bind_in(scope, name, Binding::Other);
    }

    pub(super) fn occurrence(&mut self, name: &'a ast::ExprName, use_: Use) {
        if name.ctx != ExprContext::Load {
            return;
        }
        self.occurrences.push(Occurrence {
            offset: name.range().start(),
            scope: self.current,
            name: name.id.as_str(),
            use_,
            builtin: self.builtin,
        });
    }

    pub(super) fn body(&mut self, body: &'a [ast::Stmt]) {
        for stmt in body {
            self.stmt(stmt);
        }
    }

    pub(super) fn parameters(&mut self, parameters: &'a ast::Parameters) {
        for p in parameters
            .posonlyargs
            .iter()
            .chain(parameters.args.iter())
            .chain(parameters.kwonlyargs.iter())
        {
            self.bind(p.parameter.name.as_str(), Binding::Other);
        }
        for p in parameters.vararg.iter().chain(parameters.kwarg.iter()) {
            self.bind(p.name.as_str(), Binding::Other);
        }
    }

    // Defaults are evaluated, once, in the enclosing scope, and kept by the function.
    pub(super) fn parameter_defaults(&mut self, parameters: &'a ast::Parameters) {
        for p in parameters
            .posonlyargs
            .iter()
            .chain(parameters.args.iter())
            .chain(parameters.kwonlyargs.iter())
        {
            if let Some(default) = &p.default {
                self.expr(default, Use::Escapes);
            }
        }
    }

    // An assignment, `for`, `with` or `del` target.
    pub(super) fn target(&mut self, target: &'a Expr) {
        match target {
            Expr::Name(n) => self.bind(n.id.as_str(), Binding::Other),
            Expr::Tuple(t) => t.elts.iter().for_each(|e| self.target(e)),
            Expr::List(l) => l.elts.iter().for_each(|e| self.target(e)),
            Expr::Starred(s) => self.target(&s.value),
            // `x[k] = v`, `x.a = v`, `del x[k]` change the object `x` refers to.
            Expr::Subscript(s) => {
                self.expr(&s.value, Use::Escapes);
                self.expr(&s.slice, Use::Safe);
            }
            Expr::Attribute(a) => {
                self.expr(&a.value, Use::Escapes);
                if let Expr::Name(base) = &*a.value {
                    self.pending_attribute_stores.push((
                        self.current,
                        base.id.as_str(),
                        a.attr.as_str(),
                    ));
                }
            }
            _ => {}
        }
    }

    pub(super) fn pattern(&mut self, pattern: &'a ast::Pattern) {
        match pattern {
            ast::Pattern::MatchValue(p) => self.expr(&p.value, Use::Safe),
            ast::Pattern::MatchSingleton(_) => {}
            ast::Pattern::MatchSequence(p) => p.patterns.iter().for_each(|q| self.pattern(q)),
            ast::Pattern::MatchMapping(p) => {
                p.keys.iter().for_each(|k| self.expr(k, Use::Safe));
                p.patterns.iter().for_each(|q| self.pattern(q));
                if let Some(rest) = &p.rest {
                    self.bind(rest.as_str(), Binding::Other);
                }
            }
            ast::Pattern::MatchClass(p) => {
                self.expr(&p.cls, Use::Safe);
                p.arguments.patterns.iter().for_each(|q| self.pattern(q));
                p.arguments
                    .keywords
                    .iter()
                    .for_each(|k| self.pattern(&k.pattern));
            }
            ast::Pattern::MatchStar(p) => {
                if let Some(name) = &p.name {
                    self.bind(name.as_str(), Binding::Other);
                }
            }
            ast::Pattern::MatchAs(p) => {
                if let Some(inner) = &p.pattern {
                    self.pattern(inner);
                }
                if let Some(name) = &p.name {
                    self.bind(name.as_str(), Binding::Other);
                }
            }
            ast::Pattern::MatchOr(p) => p.patterns.iter().for_each(|q| self.pattern(q)),
        }
    }
}
