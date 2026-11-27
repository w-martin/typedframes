use super::*;

impl UsageScan<'_> {
    pub(super) fn visit_expr(&mut self, expr: &Expr) {
        let at = expr.range().start();
        if let Expr::Subscript(sub) = expr {
            if self.visit_filtered_selection(sub) {
                return;
            }
        }
        match expr {
            // `frame.groupby(keys)["col"]`: the keys and the selected columns.
            Expr::Subscript(sub) if self.groupby_keys(&sub.value).is_some() => {
                self.record_groupby_selection(at, sub)
            }
            Expr::Subscript(sub) => self.visit_subscript(at, sub),
            Expr::Attribute(attr) => self.visit_attribute(at, attr),
            Expr::Call(call) => {
                // These can read or rebind ANY variable by name, so nothing about the
                // frame can be ruled out once one is called in this scope.
                self.note_dynamic_scope_call(at, call);
                if self.record_named_aggregation(call) {
                    return;
                }
                self.visit_method_call(at, call);
            }
            // A bare reference reached generically -- nothing more specific above
            // (Subscript/Attribute/Call-as-receiver/Call-argument) already recorded
            // it. Still a real use the checker can't otherwise characterize (e.g.
            // `pd.concat([df, other])`, `df + 1`, `total += df`) -- record it rather
            // than silently drop it, consistent with this module's own stated
            // philosophy of recording every observation it can, even unclassified.
            Expr::Name(name)
                if self.tainted.contains(name.id.as_str())
                    && !self.is_suppressed(name.id.as_str()) =>
            {
                self.record_other(at, "referenced", Consumption::Unknown);
            }
            Expr::Name(_) => {}
            // Same treatment as `FunctionDef`/`ClassDef` in `visit_stmt`: a lambda
            // body is its own scope, not walked (its parameter defaults are not).
            Expr::Lambda(lambda) => {
                if let Some(parameters) = &lambda.parameters {
                    self.visit_parameter_defaults(parameters);
                }
            }
            Expr::Named(named) => self.visit_walrus(named),
            Expr::Dict(dict) => self.visit_dict(dict),
            Expr::ListComp(comp) => self.visit_eager_comprehension(&comp.generators, &[&comp.elt]),
            Expr::SetComp(comp) => self.visit_eager_comprehension(&comp.generators, &[&comp.elt]),
            Expr::DictComp(comp) => self.visit_dict_comp(comp),
            // A generator expression is its own scope, like `Lambda`, run whenever it is
            // consumed -- except that the iterable of its OUTERMOST `for` clause is
            // evaluated here (e.g. `sum(x for x in df["a"])` still reads `"a"`).
            Expr::Generator(comp) => self.visit_outer_comprehension_iter(&comp.generators),
            Expr::Slice(slice) => self.visit_slice(slice),
            Expr::FString(fstring) => self.visit_interpolated(fstring.value.elements()),
            Expr::TString(tstring) => self.visit_interpolated(tstring.value.elements()),
            _ => {
                for child in plain_children(expr) {
                    self.visit_expr(child);
                }
            }
        }
    }

    // A walrus target (`y := df`) binds a real name in the ENCLOSING scope -- unlike
    // `Lambda`, this isn't a nested scope, so it needs the same alias-vs-use handling
    // `Stmt::Assign` gets, not a skip.
    fn visit_walrus(&mut self, named: &ast::ExprNamed) {
        self.visit_unless_tainted(&named.value);
        if let Expr::Name(t) = &*named.target {
            let forwards = self.forwards(&named.value);
            self.rebind_name(t.range().start(), t.id.as_str(), forwards);
        }
    }

    fn visit_dict_comp(&mut self, comp: &ruff_python_ast::ExprDictComp) {
        let mut elements: Vec<&Expr> = Vec::with_capacity(2);
        if let Some(key) = &comp.key {
            elements.push(key);
        }
        elements.push(&comp.value);
        self.visit_eager_comprehension(&comp.generators, &elements);
    }

    fn visit_dict(&mut self, dict: &ruff_python_ast::ExprDict) {
        for item in &dict.items {
            if let Some(key) = &item.key {
                self.visit_expr(key);
            }
            self.visit_expr(&item.value);
        }
    }

    fn visit_attribute(&mut self, at: TextSize, attr: &ruff_python_ast::ExprAttribute) {
        if let Some(receiver) = self.tainted_name(&attr.value) {
            if !self.is_suppressed(receiver) {
                let name = attr.attr.as_str();
                self.record_other(at, format!("`.{name}`"), classify_name(name));
            }
        } else {
            self.visit_expr(&attr.value);
        }
    }

    fn visit_slice(&mut self, slice: &ruff_python_ast::ExprSlice) {
        if let Some(lower) = &slice.lower {
            self.visit_expr(lower);
        }
        if let Some(upper) = &slice.upper {
            self.visit_expr(upper);
        }
        if let Some(step) = &slice.step {
            self.visit_expr(step);
        }
    }

    fn record_groupby_selection(&mut self, at: TextSize, sub: &ruff_python_ast::ExprSubscript) {
        let keys = self.groupby_keys(&sub.value).unwrap_or_default();
        match crate::ast_extract::extract_string_list_or_single(&sub.slice) {
            Some(selected) => {
                for column in keys.into_iter().chain(selected) {
                    self.record_column(at, column);
                }
            }
            None => self.record_other(at, "`.groupby()`", Consumption::Unknown),
        }
    }

    fn note_dynamic_scope_call(&mut self, at: TextSize, call: &ruff_python_ast::ExprCall) {
        if let Expr::Name(f) = &*call.func {
            if matches!(
                f.id.as_str(),
                "eval" | "exec" | "locals" | "globals" | "vars"
            ) {
                self.record_other(
                    at,
                    format!("`{}()` can reach any variable", f.id),
                    Consumption::Unknown,
                );
            }
        }
    }

    fn visit_subscript(&mut self, at: TextSize, sub: &ruff_python_ast::ExprSubscript) {
        if let Some(receiver) = self.tainted_name(&sub.value) {
            match crate::ast_extract::extract_string_list_or_single(&sub.slice) {
                Some(cols) => {
                    for col in cols {
                        self.record_column(at, col);
                    }
                }
                None => self.record_other(at, "this subscript", Consumption::Unknown),
            }
            // The whole subscript is already summarized by the observation
            // just recorded above (specific columns, or "this subscript" for
            // an unrecognized shape) -- suppress a bare self-reference found
            // while walking the slice (`df[df > 0]`, `df[df.values]`), or one
            // subscript operation double-counts as two separate uses. A
            // MORE SPECIFIC nested reference (`df[df["age"] > 30]`'s inner
            // `df["age"]`) is unaffected -- it's a real, distinct fact (which
            // column), not a restatement of this one.
            let previous = self.suppressed_self_reference.replace(receiver.to_string());
            self.visit_expr(&sub.slice);
            self.suppressed_self_reference = previous;
        } else {
            self.visit_expr(&sub.value);
            self.visit_expr(&sub.slice);
        }
    }

    fn visit_method_call(&mut self, at: TextSize, call: &ruff_python_ast::ExprCall) {
        match &*call.func {
            // A method call ON the tainted var: `var.method(...)` -- one
            // "other use" for the whole call, not also the generic
            // attribute-access case above.
            Expr::Attribute(attr) => match self.tainted_name(&attr.value) {
                // Polars `frame.select("a", pl.col("b"))`: exactly those columns.
                Some(receiver)
                    if !self.is_suppressed(receiver)
                        && attr.attr.as_str() == "select"
                        && call.arguments.keywords.is_empty()
                        && !call.arguments.args.is_empty()
                        && call.arguments.args.iter().all(|a| {
                            crate::ast_extract::literal_key_columns(a).is_some_and(|cs| {
                                cs.iter().all(|c| !c.starts_with('^') && c != "*")
                            })
                        }) =>
                {
                    for arg in &call.arguments.args {
                        for column in
                            crate::ast_extract::literal_key_columns(arg).unwrap_or_default()
                        {
                            self.record_column(at, column);
                        }
                    }
                }
                Some(receiver)
                    if !self.is_suppressed(receiver)
                        && ROW_DEPENDENT_METHODS.contains(&attr.attr.as_str())
                        && row_op_reads(call).is_none() =>
                {
                    let name = attr.attr.as_str();
                    self.record_other(
                        at,
                        format!(
                            "`.{name}()` without a literal `subset=` considers every column, so narrowing would change which rows it keeps"
                        ),
                        Consumption::All,
                    );
                }
                Some(receiver) if !self.is_suppressed(receiver) => {
                    let name = attr.attr.as_str();
                    self.record_other(at, format!("`.{name}()`"), classify_name(name));
                }
                Some(_) => {}
                None => self.visit_expr(&call.func),
            },
            _ => self.visit_expr(&call.func),
        }
        self.visit_function_call(at, call);
    }

    fn visit_function_call(&mut self, at: TextSize, call: &ruff_python_ast::ExprCall) {
        let callee = match &*call.func {
            Expr::Name(f) => Some(f),
            _ => None,
        };
        let builtin = callee.filter(|f| self.helpers.is_unshadowed_builtin(f));
        let passed = match (builtin, &*call.func) {
            (Some(f), _) if matches!(f.id.as_str(), "print" | "display") => {
                format!("{OUTPUT_USE_PREFIX}`{}()`", f.id)
            }
            // Something else named `len` could do anything with the frame.
            (None, Expr::Name(f)) if f.id.as_str() == "len" => {
                "passed to a function named `len()` that isn't the builtin".to_string()
            }
            _ => passed_to(&call.func),
        };
        let resolved = self.summaries_for(&call.func);
        let callee_class = self.helpers.callee_class(&call.func);
        let via = resolved.as_ref().map(|(name, _)| name.as_str());
        let summaries = resolved.as_ref().map(|(_, all)| all);
        for (position, arg) in call.arguments.args.iter().enumerate() {
            let displayed = passed
                .starts_with(OUTPUT_USE_PREFIX)
                .then(|| self.display_only_use(arg))
                .flatten();
            if let Some(name) = displayed {
                self.record_other(
                    arg.range().start(),
                    format!("{passed} of `.{name}`"),
                    classify_name(&name),
                );
            } else if self.is_tainted(arg) {
                let summary = summaries.and_then(|all| all.iter().find(|p| p.position == position));
                self.passed_argument(at, &passed, via, summary, callee_class);
            } else {
                self.visit_expr(arg);
            }
        }
        for kw in &call.arguments.keywords {
            if self.is_tainted(&kw.value) {
                let summary = kw.arg.as_ref().and_then(|name| {
                    summaries.and_then(|all| all.iter().find(|p| p.name == name.as_str()))
                });
                self.passed_argument(at, &passed, via, summary, callee_class);
            } else {
                self.visit_expr(&kw.value);
            }
        }
    }
}

// The sub-expressions of an expression that only combines or wraps others (arithmetic,
// comparisons, collections, conditionals, `yield`, `await`), in evaluation order.
fn plain_children(expr: &Expr) -> Vec<&Expr> {
    match expr {
        Expr::Set(set) => set.elts.iter().collect(),
        Expr::Yield(y) => y.value.iter().map(|value| &**value).collect(),
        Expr::YieldFrom(yf) => vec![&*yf.value],
        Expr::BinOp(binop) => vec![&*binop.left, &*binop.right],
        Expr::BoolOp(boolop) => boolop.values.iter().collect(),
        Expr::UnaryOp(unary) => vec![&*unary.operand],
        Expr::Compare(compare) => std::iter::once(&*compare.left)
            .chain(compare.comparators.iter())
            .collect(),
        Expr::List(list) => list.elts.iter().collect(),
        Expr::Tuple(tuple) => tuple.elts.iter().collect(),
        Expr::Starred(starred) => vec![&*starred.value],
        Expr::If(if_expr) => vec![&*if_expr.test, &*if_expr.body, &*if_expr.orelse],
        Expr::Await(awaited) => vec![&*awaited.value],
        _ => Vec::new(),
    }
}
