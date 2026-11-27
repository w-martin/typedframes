use super::*;

impl<'a> Collector<'a> {
    pub(super) fn expr(&mut self, expr: &'a Expr, use_: Use) {
        match expr {
            Expr::Name(n) => self.occurrence(n, use_),
            Expr::Attribute(a) => {
                self.expr_attribute(use_, a);
            }
            // The result of a subscript is an element (or a copy of part) of its base, so the
            // base is exposed at most to the interior: a mutable element lets it be changed
            // unseen, an immutable one can't. Consumed `Safe` stays `Safe`.
            Expr::Subscript(s) => {
                self.expr(&s.value, use_.min(Use::Interior));
                self.expr(&s.slice, Use::Safe);
            }
            Expr::Slice(s) => {
                s.lower
                    .iter()
                    .chain(s.upper.iter())
                    .chain(s.step.iter())
                    .for_each(|e| self.expr(e, Use::Safe));
            }
            Expr::Call(c) => self.call(c),
            Expr::BinOp(b) => {
                self.expr(&b.left, Use::Interior);
                self.expr(&b.right, Use::Interior);
            }
            Expr::UnaryOp(u) => self.expr(&u.operand, Use::Safe),
            // `a or b` evaluates to one of its operands.
            Expr::BoolOp(b) => b.values.iter().for_each(|v| self.expr(v, use_)),
            Expr::Compare(c) => {
                self.expr(&c.left, Use::Safe);
                c.comparators.iter().for_each(|e| self.expr(e, Use::Safe));
            }
            Expr::If(i) => {
                self.expr(&i.test, Use::Safe);
                self.expr(&i.body, use_);
                self.expr(&i.orelse, use_);
            }
            Expr::List(l) => self.elements(&l.elts),
            Expr::Tuple(t) => self.elements(&t.elts),
            Expr::Set(s) => self.elements(&s.elts),
            Expr::Dict(d) => {
                self.expr_dict(d);
            }
            Expr::Starred(s) => self.expr(&s.value, Use::Escapes),
            Expr::FString(f) => self.interpolations(
                f.value
                    .as_slice()
                    .iter()
                    .filter_map(|p| match p {
                        ast::FStringPart::FString(f) => Some(f.elements.iter()),
                        ast::FStringPart::Literal(_) => None,
                    })
                    .flatten(),
            ),
            Expr::TString(t) => self.interpolations(t.value.elements()),
            Expr::Lambda(l) => {
                self.expr_lambda(l);
            }
            Expr::ListComp(c) => self.comprehension(&c.generators, &[(&c.elt, Use::Escapes)]),
            Expr::SetComp(c) => self.comprehension(&c.generators, &[(&c.elt, Use::Escapes)]),
            Expr::Generator(c) => self.comprehension(&c.generators, &[(&c.elt, Use::Escapes)]),
            Expr::DictComp(c) => {
                self.expr_dict_comp(c);
            }
            Expr::Await(a) => self.expr(&a.value, Use::Escapes),
            Expr::Yield(y) => y.value.iter().for_each(|v| self.expr(v, Use::Escapes)),
            Expr::YieldFrom(y) => self.expr(&y.value, Use::Escapes),
            Expr::Named(n) => {
                self.expr(&n.value, Use::Escapes);
                if let Expr::Name(target) = &*n.target {
                    self.bind_walrus(target.id.as_str());
                }
            }
            Expr::StringLiteral(_)
            | Expr::BytesLiteral(_)
            | Expr::NumberLiteral(_)
            | Expr::BooleanLiteral(_)
            | Expr::NoneLiteral(_)
            | Expr::EllipsisLiteral(_)
            | Expr::IpyEscapeCommand(_) => {}
        }
    }

    fn expr_dict_comp(&mut self, c: &'a ruff_python_ast::ExprDictComp) {
        let mut parts: Vec<(&'a Expr, Use)> = Vec::new();
        if let Some(key) = &c.key {
            parts.push((key, Use::Safe));
        }
        parts.push((&c.value, Use::Escapes));
        self.comprehension(&c.generators, &parts);
    }

    fn expr_dict(&mut self, d: &'a ruff_python_ast::ExprDict) {
        for item in &d.items {
            match &item.key {
                Some(key) => {
                    self.expr(key, Use::Safe);
                    self.expr(&item.value, Use::Escapes);
                }
                None => self.expr(&item.value, Use::Interior),
            }
        }
    }

    fn expr_lambda(&mut self, l: &'a ruff_python_ast::ExprLambda) {
        if let Some(parameters) = &l.parameters {
            self.parameter_defaults(parameters);
        }
        let outer = self.current;
        self.push_scope(ScopeKind::Function);
        if let Some(parameters) = &l.parameters {
            self.parameters(parameters);
        }
        self.expr(&l.body, Use::Escapes);
        self.current = outer;
    }

    fn expr_attribute(&mut self, use_: Use, a: &'a ruff_python_ast::ExprAttribute) {
        if let Expr::Name(base) = &*a.value {
            if a.attr.as_str() == "__dict__" {
                self.dynamic_class_targets
                    .push((self.current, base.id.as_str()));
            }
            if a.ctx == ExprContext::Load {
                self.attribute_occurrences.push(AttributeOccurrence {
                    range: a.range(),
                    scope: self.current,
                    base: base.id.as_str(),
                    attr: a.attr.as_str(),
                    use_,
                });
            }
        }
        self.expr(&a.value, Use::Escapes);
    }

    // List/tuple/set display: an element is stored in the new container; `*x` only
    // reads x's elements.
    pub(super) fn elements(&mut self, elts: &'a [Expr]) {
        for elt in elts {
            match elt {
                Expr::Starred(s) => self.expr(&s.value, Use::Interior),
                other => self.expr(other, Use::Escapes),
            }
        }
    }

    fn interpolations<'e>(
        &mut self,
        elements: impl Iterator<Item = &'a ast::InterpolatedStringElement> + 'e,
    ) where
        'a: 'e,
    {
        for element in elements {
            if let ast::InterpolatedStringElement::Interpolation(interp) = element {
                self.expr(&interp.expression, Use::Safe);
                if let Some(spec) = &interp.format_spec {
                    self.interpolations(spec.elements.iter());
                }
            }
        }
    }

    // The first iterable is evaluated in the enclosing scope; everything else in the
    // comprehension's own.
    pub(super) fn comprehension(
        &mut self,
        generators: &'a [ast::Comprehension],
        elements: &[(&'a Expr, Use)],
    ) {
        if let Some(first) = generators.first() {
            self.expr(&first.iter, Use::Interior);
        }
        let outer = self.current;
        self.push_scope(ScopeKind::Comprehension);
        for (i, generator) in generators.iter().enumerate() {
            self.target(&generator.target);
            if i > 0 {
                self.expr(&generator.iter, Use::Interior);
            }
            for condition in &generator.ifs {
                self.expr(condition, Use::Safe);
            }
        }
        for (element, use_) in elements {
            self.expr(element, *use_);
        }
        self.current = outer;
    }

    pub(super) fn call(&mut self, call: &'a ast::ExprCall) {
        let mut arg_use = Use::Escapes;
        let mut builtin: Option<&'static str> = None;
        match &*call.func {
            Expr::Name(f) => {
                self.occurrence(f, Use::Safe);
                let name = f.id.as_str();
                if matches!(name, "globals" | "vars" | "exec" | "eval" | "locals") {
                    self.dynamic_namespace = true;
                }
                // These can set attributes on a class without naming them.
                if matches!(name, "setattr" | "delattr" | "vars") {
                    if let Some(Expr::Name(target)) = call.arguments.args.first() {
                        self.dynamic_class_targets
                            .push((self.current, target.id.as_str()));
                    }
                }
                if let Some(b) = SAFE_BUILTINS.iter().find(|b| **b == name) {
                    (arg_use, builtin) = (Use::Safe, Some(*b));
                } else if let Some(b) = INTERIOR_BUILTINS.iter().find(|b| **b == name) {
                    (arg_use, builtin) = (Use::Interior, Some(*b));
                } else if let Some(b) = LOAD_FUNCTIONS.iter().find(|b| **b == name) {
                    // Trusted not to change what it is given, unless the file defines its own.
                    (arg_use, builtin) = (Use::Safe, Some(*b));
                }
            }
            Expr::Attribute(a) => {
                let method = a.attr.as_str();
                // `pd.concat(...)`, `pl.col(...)`: a call through the pandas/polars module.
                let module_call =
                    matches!(&*a.value, Expr::Name(n) if LOAD_MODULES.contains(&n.id.as_str()));
                let (receiver, args) = if LOAD_FUNCTIONS.contains(&method)
                    || SAFE_METHODS.contains(&method)
                    || module_call
                {
                    (Use::Safe, Use::Safe)
                } else if method == "join" {
                    (Use::Safe, Use::Interior)
                } else if INTERIOR_METHODS.contains(&method) {
                    (Use::Interior, Use::Safe)
                } else if FRAME_METHODS.contains(&method) {
                    // The receiver is a frame (or a static value it makes no sense to call
                    // this on): either way it is not the arguments' concern.
                    (Use::Escapes, Use::Safe)
                } else {
                    (Use::Escapes, Use::Escapes)
                };
                // `cfg.cols(...)` may be a call to a function of an imported module.
                if let Expr::Name(base) = &*a.value {
                    self.attribute_occurrences.push(AttributeOccurrence {
                        range: a.range(),
                        scope: self.current,
                        base: base.id.as_str(),
                        attr: a.attr.as_str(),
                        use_: Use::Safe,
                    });
                }
                self.expr(&a.value, receiver);
                arg_use = args;
            }
            other => self.expr(other, Use::Escapes),
        }
        let outer = self.builtin.replace_with(builtin);
        for arg in call.arguments.args.iter() {
            match arg {
                Expr::Starred(s) => self.expr(&s.value, Use::Escapes),
                other => self.expr(other, arg_use),
            }
        }
        for keyword in call.arguments.keywords.iter() {
            let use_ = if keyword.arg.is_some() {
                arg_use
            } else {
                Use::Escapes
            };
            self.expr(&keyword.value, use_);
        }
        self.builtin = outer;
    }
}

pub(super) fn import_binding(alias: &ruff_python_ast::Alias) -> (&str, Binding<'_>) {
    let (bound, binding) = match &alias.asname {
        Some(asname) => (
            asname.as_str(),
            Binding::ImportModule(alias.name.to_string()),
        ),
        // `import a.b` binds `a`, the package, not the module.
        None if alias.name.as_str().contains('.') => (
            alias.name.as_str().split('.').next().unwrap_or(""),
            Binding::Other,
        ),
        None => (
            alias.name.as_str(),
            Binding::ImportModule(alias.name.to_string()),
        ),
    };
    (bound, binding)
}

trait ReplaceWith<T> {
    fn replace_with(&mut self, value: T) -> T;
}

impl<T> ReplaceWith<Option<T>> for Option<T> {
    fn replace_with(&mut self, value: Option<T>) -> Option<T> {
        std::mem::replace(self, value)
    }
}

// Every load of a name in the module, other than in annotations and type parameters
// (which the collector does not classify). Used to check the collector missed none: a
// missed use could hide a mutation.
pub(super) struct AllNameLoads {
    pub(super) offsets: HashSet<TextSize>,
}

impl<'a> Visitor<'a> for AllNameLoads {
    fn visit_annotation(&mut self, _expr: &'a Expr) {}
    fn visit_type_params(&mut self, _type_params: &'a ast::TypeParams) {}
    // `type X = ...` is a type-level alias, like an annotation.
    fn visit_stmt(&mut self, stmt: &'a ast::Stmt) {
        if !matches!(stmt, ast::Stmt::TypeAlias(_)) {
            ast_visitor::walk_stmt(self, stmt);
        }
    }
    fn visit_expr(&mut self, expr: &'a Expr) {
        if let Expr::Name(n) = expr {
            if n.ctx == ExprContext::Load {
                self.offsets.insert(n.range().start());
            }
        }
        ast_visitor::walk_expr(self, expr);
    }
}
