use super::*;

impl Linter {
    pub(super) fn visit_expr(&self, expr: &Expr, errors: &mut Vec<LintError>) {
        match expr {
            Expr::Attribute(attr) => self.visit_attribute(errors, attr),
            // `frame.groupby(keys)["col"]` / `[["a", "b"]]`: the selection is read off the frame.
            Expr::Subscript(subscript) => self.visit_subscript(errors, subscript),
            Expr::Call(call) => self.visit_call(errors, call),
            Expr::Dict(dict) => self.visit_dict(errors, dict),
            Expr::Lambda(lambda) => self.visit_lambda(errors, lambda),
            Expr::ListComp(comp) => {
                self.visit_comprehension(&comp.generators, &[&comp.elt], errors)
            }
            Expr::SetComp(comp) => self.visit_comprehension(&comp.generators, &[&comp.elt], errors),
            Expr::Generator(comp) => {
                self.visit_comprehension(&comp.generators, &[&comp.elt], errors)
            }
            Expr::DictComp(comp) => {
                let mut bodies: Vec<&Expr> = comp.key.iter().map(|key| &**key).collect();
                bodies.push(&comp.value);
                self.visit_comprehension(&comp.generators, &bodies, errors)
            }
            // Column accesses nested inside arithmetic, boolean, comparison, or
            // literal-collection expressions (`df["a"] + df["b"]`, `df["x"] > 0`,
            // `[df["a"], df["b"]]`, …) must still be recursed into — otherwise any
            // access wrapped in one of these silently escapes validation entirely,
            // regardless of whether it's schema-based, inferred, or contract-based.
            _ => {
                for child in nested_expressions(expr) {
                    self.visit_expr(child, errors);
                }
            }
        }
    }

    fn visit_dict(&self, errors: &mut Vec<LintError>, dict: &ruff_python_ast::ExprDict) {
        for item in &dict.items {
            if let Some(key) = &item.key {
                self.visit_expr(key, errors);
            }
            self.visit_expr(&item.value, errors);
        }
    }

    fn visit_lambda(&self, errors: &mut Vec<LintError>, lambda: &ruff_python_ast::ExprLambda) {
        let mut bound = Vec::new();
        if let Some(parameters) = &lambda.parameters {
            for p in parameters.iter() {
                bound.push((p.name().to_string(), ""));
            }
        }
        if !self.shadows_a_frame(&bound) {
            self.visit_expr(&lambda.body, errors);
        }
    }

    fn visit_call(&self, errors: &mut Vec<LintError>, call: &ruff_python_ast::ExprCall) {
        self.check_frame_method_arguments(call, errors);
        // `print(f(frame).shape)`: a contract call nested in another expression.
        if !self.contracts.is_empty() || !self.method_agreement.is_empty() {
            let (line, col) = self.source_location(call.range().start());
            self.check_call_requirements(call, line, col, errors);
        }
        for arg in call.arguments.args.iter() {
            self.visit_expr(arg, errors);
        }
        // Keyword arguments (e.g. `.assign(amount_vat=public["amount"] * 1.2)`)
        // carry column accesses just as often as positional ones — must be
        // checked too, not just skipped as "the method name".
        for kw in call.arguments.keywords.iter() {
            self.visit_expr(&kw.value, errors);
        }
        // When the callee is `receiver.method(...)`, do not check the method name
        // as a column access — only recurse into the receiver so that any column
        // accesses nested there (e.g. `df.col.method()`) are still found.
        if let Expr::Attribute(attr) = &*call.func {
            self.visit_expr(&attr.value, errors);
        } else {
            self.visit_expr(&call.func, errors);
        }
    }

    fn visit_attribute(&self, errors: &mut Vec<LintError>, attr: &ruff_python_ast::ExprAttribute) {
        if let Expr::Name(name) = &*attr.value {
            self.visit_attribute_of_name(errors, attr, name);
        }
        self.visit_expr(&attr.value, errors);
    }

    fn visit_attribute_of_name(
        &self,
        errors: &mut Vec<LintError>,
        attr: &ruff_python_ast::ExprAttribute,
        name: &ruff_python_ast::ExprName,
    ) {
        if let Some((schema_name, defined_line)) = self.variables.get(name.id.as_str()) {
            self.check_schema_attribute(errors, attr, schema_name, defined_line);
        }
    }

    fn check_schema_attribute(
        &self,
        errors: &mut Vec<LintError>,
        attr: &ruff_python_ast::ExprAttribute,
        schema_name: &String,
        defined_line: &usize,
    ) {
        let attr_name = attr.attr.as_str();
        if !RESERVED_METHODS.contains(&attr_name) {
            self.check_attribute_column(errors, attr, schema_name, defined_line, attr_name);
        }
    }

    fn check_attribute_column(
        &self,
        errors: &mut Vec<LintError>,
        attr: &ruff_python_ast::ExprAttribute,
        schema_name: &String,
        defined_line: &usize,
        attr_name: &str,
    ) {
        if self.schema_is_unresolved(schema_name) {
            let (line, col) = self.source_location(attr.range().start());
            if !self.is_known_on_open_leg(schema_name, attr_name)
                && self.first_unverifiable_report(schema_name)
            {
                errors.push(LintError {
                    line,
                    col,
                    code: CODE_UNVERIFIABLE_COLUMN.to_string(),
                    message: self.unverifiable_message(attr_name, schema_name),
                    severity: "warning".to_string(),
                });
            }
        } else if let Some(columns) = self.schemas.get(schema_name) {
            if !self.schema_has_column(schema_name, attr_name) {
                let (line, col) = self.source_location(attr.range().start());
                let schema_display = self.schema_display(schema_name, *defined_line);
                let mut message = format!(
                    "Column '{}' does not exist in {}",
                    attr_name, schema_display
                );
                if let Some(suggestion) = find_best_match(attr_name, columns) {
                    message.push_str(&format!(" (did you mean '{}'?)", suggestion));
                }
                errors.push(LintError {
                    line,
                    col,
                    code: CODE_UNKNOWN_COLUMN.to_string(),
                    message,
                    severity: "error".to_string(),
                });
            }
        }
    }

    fn visit_subscript(
        &self,
        errors: &mut Vec<LintError>,
        subscript: &ruff_python_ast::ExprSubscript,
    ) {
        if let Expr::Call(grouped) = &*subscript.value {
            self.check_groupby_selection(errors, subscript, grouped);
        }
        self.check_subscript_read(errors, subscript);
    }

    fn check_groupby_selection(
        &self,
        errors: &mut Vec<LintError>,
        subscript: &ruff_python_ast::ExprSubscript,
        grouped: &ruff_python_ast::ExprCall,
    ) {
        if let Expr::Attribute(group_attr) = &*grouped.func {
            if group_attr.attr.as_str() == "groupby" {
                if let Some(names) = ast_extract::extract_string_list_or_single(&subscript.slice) {
                    let at = subscript.range().start();
                    self.check_names_against(&group_attr.value, &names, at, errors);
                    self.check_names_against_open(&group_attr.value, &names, at, errors);
                }
            }
        }
    }

    fn check_subscript_read(
        &self,
        errors: &mut Vec<LintError>,
        subscript: &ruff_python_ast::ExprSubscript,
    ) {
        if let Expr::Name(name) = &*subscript.value {
            self.check_subscript_of_name(errors, subscript, name);
        }
        self.check_column_list_read(errors, subscript);
    }

    fn check_subscript_of_name(
        &self,
        errors: &mut Vec<LintError>,
        subscript: &ruff_python_ast::ExprSubscript,
        name: &ruff_python_ast::ExprName,
    ) {
        if let Some((schema_name, defined_line)) = self.variables.get(name.id.as_str()) {
            self.check_subscript_column(errors, subscript, schema_name, defined_line);
        }
    }

    fn check_subscript_column(
        &self,
        errors: &mut Vec<LintError>,
        subscript: &ruff_python_ast::ExprSubscript,
        schema_name: &String,
        defined_line: &usize,
    ) {
        if let Some(col_name) = ast_extract::extract_string_literal(&subscript.slice) {
            self.check_column_read(errors, subscript, schema_name, defined_line, col_name);
        }
    }

    fn check_column_read(
        &self,
        errors: &mut Vec<LintError>,
        subscript: &ruff_python_ast::ExprSubscript,
        schema_name: &String,
        defined_line: &usize,
        col_name: &str,
    ) {
        if self.schema_is_unresolved(schema_name) {
            let (line, col) = self.source_location(subscript.range().start());
            if !self.is_known_on_open_leg(schema_name, col_name) {
                self.unknown_frame_reads
                    .borrow_mut()
                    .push((line, col, col_name.to_string()));
            }
            if !self.is_known_on_open_leg(schema_name, col_name)
                && self.first_unverifiable_report(schema_name)
            {
                errors.push(LintError {
                    line,
                    col,
                    code: CODE_UNVERIFIABLE_COLUMN.to_string(),
                    message: self.unverifiable_message(col_name, schema_name),
                    severity: "warning".to_string(),
                });
            }
        } else if let Some(columns) = self.schemas.get(schema_name) {
            if !self.schema_has_column(schema_name, col_name) {
                let (line, col) = self.source_location(subscript.range().start());
                let schema_display = self.schema_display(schema_name, *defined_line);
                let mut message =
                    format!("Column '{}' does not exist in {}", col_name, schema_display);
                if let Some(suggestion) = find_best_match(col_name, columns) {
                    message.push_str(&format!(" (did you mean '{}'?)", suggestion));
                }
                errors.push(LintError {
                    line,
                    col,
                    code: CODE_UNKNOWN_COLUMN.to_string(),
                    message,
                    severity: "error".to_string(),
                });
            }
        }
    }

    fn check_column_list_read(
        &self,
        errors: &mut Vec<LintError>,
        subscript: &ruff_python_ast::ExprSubscript,
    ) {
        if let (Expr::Name(name), Some(column), ast::ExprContext::Load) = (
            &*subscript.value,
            ast_extract::extract_string_literal(&subscript.slice),
            subscript.ctx,
        ) {
            self.check_open_frame_read(name.id.as_str(), column, subscript.range().start(), errors);
        }
        if let (Expr::Name(name), Expr::List(_), ast::ExprContext::Load) =
            (&*subscript.value, &*subscript.slice, subscript.ctx)
        {
            self.note_unknown_list_reads(name.id.as_str(), &subscript.slice);
        }
        if let (Expr::Attribute(indexer), Expr::Tuple(pair), ast::ExprContext::Load) =
            (&*subscript.value, &*subscript.slice, subscript.ctx)
        {
            self.check_subscript_of_attribute(errors, subscript, indexer, pair);
        }
        if let Some(names) = ast_extract::extract_string_list(&subscript.slice) {
            let is_row_indexer = matches!(
                &*subscript.value,
                Expr::Attribute(a) if matches!(a.attr.as_str(), "loc" | "iloc")
            );
            if !is_row_indexer {
                self.check_names_against(
                    &subscript.value,
                    &names,
                    subscript.range().start(),
                    errors,
                );
                if subscript.ctx == ast::ExprContext::Load {
                    self.check_names_against_open(
                        &subscript.value,
                        &names,
                        subscript.range().start(),
                        errors,
                    );
                }
            }
        }
        self.visit_expr(&subscript.value, errors);
        self.visit_expr(&subscript.slice, errors);
    }

    fn check_subscript_of_attribute(
        &self,
        errors: &mut Vec<LintError>,
        subscript: &ruff_python_ast::ExprSubscript,
        indexer: &ruff_python_ast::ExprAttribute,
        pair: &ruff_python_ast::ExprTuple,
    ) {
        if let (true, Expr::Name(frame), [_, columns]) = (
            indexer.attr.as_str() == "loc",
            &*indexer.value,
            pair.elts.as_slice(),
        ) {
            if let Some(names) = ast_extract::extract_string_list_or_single(columns) {
                for name in &names {
                    self.check_open_frame_read(
                        frame.id.as_str(),
                        name,
                        subscript.range().start(),
                        errors,
                    );
                }
                self.check_names_against(&indexer.value, &names, subscript.range().start(), errors);
            }
        }
    }

    // Whether any of `bound` (names a lambda or comprehension binds) hides a tracked frame.
    fn shadows_a_frame(&self, bound: &[(String, &'static str)]) -> bool {
        bound
            .iter()
            .any(|(name, _)| self.variables.contains_key(name))
    }

    // The first iterable is evaluated in the enclosing scope; the rest runs with the loop
    // targets bound, so it is skipped whenever one of them hides a tracked frame.
    pub(super) fn visit_comprehension(
        &self,
        generators: &[ast::Comprehension],
        bodies: &[&Expr],
        errors: &mut Vec<LintError>,
    ) {
        let Some(first) = generators.first() else {
            return;
        };
        self.visit_expr(&first.iter, errors);
        let mut bound = Vec::new();
        for generator in generators {
            collect_name_targets(&generator.target, "", &mut bound);
        }
        if self.shadows_a_frame(&bound) {
            return;
        }
        for (index, generator) in generators.iter().enumerate() {
            if index > 0 {
                self.visit_expr(&generator.iter, errors);
            }
            for condition in &generator.ifs {
                self.visit_expr(condition, errors);
            }
        }
        for body in bodies {
            self.visit_expr(body, errors);
        }
    }
}

// The expressions an arithmetic, boolean, comparison, collection, conditional, f-string or
// wrapping expression evaluates, in order.
fn nested_expressions(expr: &Expr) -> Vec<&Expr> {
    match expr {
        Expr::BinOp(binop) => vec![&*binop.left, &*binop.right],
        Expr::BoolOp(boolop) => boolop.values.iter().collect(),
        Expr::UnaryOp(unary) => vec![&*unary.operand],
        Expr::Compare(compare) => std::iter::once(&*compare.left)
            .chain(compare.comparators.iter())
            .collect(),
        Expr::List(list) => list.elts.iter().collect(),
        Expr::Tuple(tuple) => tuple.elts.iter().collect(),
        Expr::Set(set) => set.elts.iter().collect(),
        Expr::If(ternary) => vec![&*ternary.test, &*ternary.body, &*ternary.orelse],
        Expr::FString(fstring) => fstring
            .value
            .elements()
            .filter_map(|element| match element {
                ast::InterpolatedStringElement::Interpolation(interp) => Some(&*interp.expression),
                _ => None,
            })
            .collect(),
        Expr::Await(inner) => vec![&*inner.value],
        Expr::Starred(inner) => vec![&*inner.value],
        Expr::Named(inner) => vec![&*inner.value],
        Expr::YieldFrom(inner) => vec![&*inner.value],
        Expr::Yield(inner) => inner.value.iter().map(|value| &**value).collect(),
        _ => Vec::new(),
    }
}
