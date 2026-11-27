use super::*;

impl UsageScan<'_> {
    // `frame.rename(columns={literal}, inplace=True)` on the tracked frame: the old names are
    // source columns and the new ones are not. Declined once a new name was read earlier,
    // since that read was of a source column of the same name.
    pub(super) fn inplace_literal_rename(&mut self, expr: &Expr) -> bool {
        let Expr::Call(call) = expr else {
            return false;
        };
        let Expr::Attribute(attr) = &*call.func else {
            return false;
        };
        if attr.attr.as_str() != "rename"
            || !matches!(&*attr.value, Expr::Name(n) if self.tainted.contains(n.id.as_str()))
            || !call.arguments.args.is_empty()
        {
            return false;
        }
        let mut columns = None;
        for keyword in &call.arguments.keywords {
            match keyword.arg.as_ref().map(|a| a.as_str()) {
                Some("inplace") if matches!(&keyword.value, Expr::BooleanLiteral(b) if b.value) => {
                }
                Some("columns") => columns = Some(&keyword.value),
                _ => return false,
            }
        }
        let Some(Expr::Dict(dict)) = columns else {
            return false;
        };
        let Some(mapping) = crate::ast_extract::extract_string_dict(dict) else {
            return false;
        };
        if mapping
            .values()
            .any(|new| self.usage.accesses.iter().any(|a| &a.column == new))
        {
            return false;
        }
        let at = call.range().start();
        let mut pairs: Vec<_> = mapping.into_iter().collect();
        pairs.sort();
        let introduced: HashSet<String> = pairs.iter().map(|(_, new)| new.clone()).collect();
        for (old, new) in pairs {
            self.record_column(at, old.clone());
            if !introduced.contains(&old) {
                self.gone.insert(old);
            }
            self.gone.remove(&new);
            self.written.insert(new);
        }
        true
    }

    pub(super) fn record_column(&mut self, at: TextSize, column: String) {
        if self.written.contains(&column) {
            return;
        }
        if self.gone.contains(&column) {
            self.record_other(
                at,
                format!("reads '{column}' after a rename or drop removed it"),
                Consumption::Unknown,
            );
            return;
        }
        let line = (self.line_of)(at);
        self.usage.accesses.push(ColumnAccess {
            column,
            conditional: self.depth > 0,
            line,
            via: None,
        });
    }

    pub(super) fn record_other(
        &mut self,
        at: TextSize,
        description: impl Into<String>,
        consumes: Consumption,
    ) {
        self.record_other_via(at, description, consumes, None);
    }

    pub(super) fn record_other_via(
        &mut self,
        at: TextSize,
        description: impl Into<String>,
        consumes: Consumption,
        callee: Option<&'static str>,
    ) {
        let line = (self.line_of)(at);
        self.usage.other_uses.push(OtherUse {
            description: description.into(),
            conditional: self.depth > 0,
            consumes,
            line,
            callee,
        });
    }

    // `df.shape`, `df.columns`, `df.head()` ... (optionally subscripted), when `expr` is
    // exactly that and nothing more.
    pub(super) fn display_only_use(&self, expr: &Expr) -> Option<String> {
        let mut inner = expr;
        while let Expr::Subscript(sub) = inner {
            inner = &sub.value;
        }
        let (receiver, name) = match inner {
            Expr::Attribute(attr) => (&*attr.value, attr.attr.as_str()),
            Expr::Call(call) => match &*call.func {
                Expr::Attribute(attr) => (&*attr.value, attr.attr.as_str()),
                _ => return None,
            },
            _ => return None,
        };
        let receiver = self.tainted_name(receiver)?;
        (!self.is_suppressed(receiver) && DISPLAY_ONLY_NAMES.contains(&name))
            .then(|| name.to_string())
    }

    pub(super) fn visit_body(&mut self, body: &[Stmt]) {
        for stmt in body {
            self.visit_stmt(stmt);
        }
    }

    pub(super) fn visit_conditional_body(&mut self, body: &[Stmt]) {
        self.depth += 1;
        self.visit_body(body);
        self.depth -= 1;
    }

    // A list, set or dict comprehension runs here, once per item, so its body is part of this
    // scope; its reads are conditional, since it may run for no item at all. A target that
    // rebinds the frame's name hides it from the body.
    pub(super) fn visit_eager_comprehension(
        &mut self,
        generators: &[ast::Comprehension],
        elements: &[&Expr],
    ) {
        self.visit_outer_comprehension_iter(generators);
        if let Some(generator) = generators
            .iter()
            .find(|g| self.tainted.iter().any(|name| binds(&g.target, name)))
        {
            self.record_other(
                generator.target.range().start(),
                "rebound by a comprehension target",
                Consumption::Unknown,
            );
            return;
        }
        self.depth += 1;
        for (i, generator) in generators.iter().enumerate() {
            if i > 0 {
                if self.is_tainted(&generator.iter) {
                    self.record_other(
                        generator.iter.range().start(),
                        "iterated over directly",
                        Consumption::Unknown,
                    );
                } else {
                    self.visit_expr(&generator.iter);
                }
            }
            for condition in &generator.ifs {
                self.visit_expr(condition);
            }
        }
        for element in elements {
            self.visit_expr(element);
        }
        self.depth -= 1;
    }

    // Only the first generator's iterable runs in the enclosing scope.
    pub(super) fn visit_outer_comprehension_iter(&mut self, generators: &[ast::Comprehension]) {
        let Some(first) = generators.first() else {
            return;
        };
        if self.is_tainted(&first.iter) {
            self.record_other(
                first.iter.range().start(),
                "iterated over directly",
                Consumption::Unknown,
            );
        } else {
            self.visit_expr(&first.iter);
        }
    }

    pub(super) fn visit_interpolated<'e>(
        &mut self,
        elements: impl Iterator<Item = &'e ast::InterpolatedStringElement>,
    ) {
        for element in elements {
            if let ast::InterpolatedStringElement::Interpolation(interp) = element {
                self.visit_expr(&interp.expression);
                if let Some(spec) = &interp.format_spec {
                    self.visit_interpolated(spec.elements.iter());
                }
            }
        }
    }

    // Parameter defaults, decorators and class bases are evaluated in the ENCLOSING
    // scope when the `def`/`class` statement runs, unlike the body.
    pub(super) fn visit_parameter_defaults(&mut self, parameters: &ast::Parameters) {
        for param in parameters
            .posonlyargs
            .iter()
            .chain(parameters.args.iter())
            .chain(parameters.kwonlyargs.iter())
        {
            if let Some(default) = &param.default {
                self.visit_expr(default);
            }
        }
    }

    // The target of an assignment, `for`, `with` or `except`. `forwards` is `Some` for
    // an assignment (whether its value is, or forwards, the tracked frame) and `None`
    // for a loop/`with`/`except` target, which rebinds a name without our being able
    // to say what to.
    pub(super) fn visit_store_target(&mut self, target: &Expr, forwards: Option<bool>) {
        let at = target.range().start();
        match target {
            Expr::Name(name) => match forwards {
                Some(forwards) => self.rebind_name(at, name.id.as_str(), forwards),
                None => {
                    if self.tainted.contains(name.id.as_str()) {
                        self.record_other(
                            at,
                            format!("`{}` rebound by a loop, `with` or `except` target", name.id),
                            Consumption::Unknown,
                        );
                    }
                }
            },
            Expr::Tuple(t) => {
                for el in &t.elts {
                    self.visit_store_target(el, forwards.map(|_| false));
                }
            }
            Expr::List(l) => {
                for el in &l.elts {
                    self.visit_store_target(el, forwards.map(|_| false));
                }
            }
            Expr::Starred(s) => self.visit_store_target(&s.value, forwards.map(|_| false)),
            Expr::Subscript(sub) => {
                self.store_subscript_target(at, sub);
            }
            Expr::Attribute(attr) => {
                if self.tainted_name(&attr.value).is_some() {
                    self.record_other(
                        at,
                        format!("`.{}` assigned", attr.attr.as_str()),
                        Consumption::Unknown,
                    );
                } else {
                    self.visit_expr(&attr.value);
                }
            }
            _ => {}
        }
    }

    fn store_subscript_target(&mut self, at: TextSize, sub: &ruff_python_ast::ExprSubscript) {
        if let Some(receiver) = self.tainted_name(&sub.value) {
            // Writing into the frame: `df["new"] = ...` is not a read, so a
            // literal column here is NOT recorded as an access (doing so would
            // make a column that may not exist yet look required). Anything
            // else -- a mask, a computed key -- can touch columns we can't
            // enumerate.
            match crate::ast_extract::extract_string_list_or_single(&sub.slice) {
                None => self.record_other(
                    at,
                    "assignment through this subscript",
                    Consumption::Unknown,
                ),
                Some(columns) => {
                    self.usage.wrote_columns = true;
                    // Only an unconditional write guarantees the column exists
                    // afterwards.
                    if self.depth == 0 {
                        self.written.extend(columns);
                    }
                }
            }
            let previous = self.suppressed_self_reference.replace(receiver.to_string());
            self.visit_expr(&sub.slice);
            self.suppressed_self_reference = previous;
        } else {
            self.visit_expr(&sub.value);
            self.visit_expr(&sub.slice);
        }
    }
}
