use super::*;

impl Linter {
    pub(super) fn visit_annotated_assign(
        &mut self,
        errors: &mut Vec<LintError>,
        ann_assign: &ruff_python_ast::StmtAnnAssign,
    ) {
        let (current_line, current_col, schema_resolved) =
            self.annotated_assign_value(errors, ann_assign);

        if schema_resolved {
            self.dataframes_total += 1;
            // Every branch above resolves by writing the target's schema into
            // `self.variables`, so read it back rather than threading a name out
            // of five separate arms. An annotation always names a real schema
            // class, so this is never an open schema today -- routing it through
            // the same helper as every other counting site is what keeps the
            // open-schema tally correct by construction if that ever changes.
            let var_hint = match &*ann_assign.target {
                Expr::Name(target_name) => target_name.id.to_string(),
                _ => "df".to_string(),
            };
            let schema_name = self
                .variables
                .get(var_hint.as_str())
                .map(|(schema, _)| schema.clone())
                .unwrap_or_default();
            if !self.schemas.contains_key(&schema_name) {
                self.unresolved_schemas.insert(schema_name.clone());
                self.dataframes_unresolved += 1;
                errors.push(LintError {
                    line: current_line,
                    col: current_col,
                    code: CODE_UNRESOLVED_SCHEMA.to_string(),
                    message: format!(
                        "Schema `{schema_name}` annotating `{var_hint}` is not defined here or in any import the checker could follow, so no column on it is checked (a file checked on its own cannot follow every import; check the whole project)"
                    ),
                    severity: "warning".to_string(),
                });
            }
            self.count_typed_dataframe(&schema_name, &var_hint, current_line, current_col);
        }

        self.visit_expr(&ann_assign.target, errors);
        if let Some(value) = &ann_assign.value {
            self.visit_expr(value, errors);
        }
    }

    fn annotated_assign_value(
        &mut self,
        errors: &mut Vec<LintError>,
        ann_assign: &ruff_python_ast::StmtAnnAssign,
    ) -> (usize, usize, bool) {
        let (current_line, current_col) = self.source_location(ann_assign.range().start());
        // An annotated assignment's schema is always fully known once resolved --
        // there's no "partially typed" case here, unlike a load call whose columns
        // might not parse. Both the value-side and annotation-side checks below can
        // independently resolve the SAME variable (e.g. `df: DataFrame[S] =
        // DataFrame[S](data)`), so track one flag and count once at the end rather
        // than at each `self.variables.insert` site, to avoid double-counting one
        // statement producing one DataFrame.
        let mut schema_resolved = false;

        if let Some(value) = &ann_assign.value {
            if let Expr::Call(call) = &**value {
                self.annotated_assign_call(ann_assign, current_line, &mut schema_resolved, call);
            }
        }

        // Track schema from type annotation
        self.track_annotation_schema(ann_assign, current_line, &mut schema_resolved);

        if !matches!(&*ann_assign.target, Expr::Name(_)) {
            self.warn_unresolved_attribute_schema(ann_assign, current_line, current_col, errors);
        }
        (current_line, current_col, schema_resolved)
    }

    fn annotated_assign_call(
        &mut self,
        ann_assign: &ruff_python_ast::StmtAnnAssign,
        current_line: usize,
        schema_resolved: &mut bool,
        call: &ruff_python_ast::ExprCall,
    ) {
        if let Expr::Subscript(subscript) = &*call.func {
            self.annotated_assign_constructor(ann_assign, current_line, schema_resolved, subscript);
        } else if let Expr::Attribute(attr) = &*call.func {
            self.annotated_assign_method_call(ann_assign, current_line, schema_resolved, attr);
        }
    }

    fn annotated_assign_constructor(
        &mut self,
        ann_assign: &ruff_python_ast::StmtAnnAssign,
        current_line: usize,
        schema_resolved: &mut bool,
        subscript: &ruff_python_ast::ExprSubscript,
    ) {
        if let Expr::Name(name) = &*subscript.value {
            self.annotated_assign_constructor_named(
                ann_assign,
                current_line,
                schema_resolved,
                subscript,
                name,
            );
        }
    }

    fn annotated_assign_constructor_named(
        &mut self,
        ann_assign: &ruff_python_ast::StmtAnnAssign,
        current_line: usize,
        schema_resolved: &mut bool,
        subscript: &ruff_python_ast::ExprSubscript,
        name: &ruff_python_ast::ExprName,
    ) {
        let type_name = name.id.as_str();
        if type_name == "DataFrame" {
            if let Expr::Name(schema_name) = &*subscript.slice {
                if let Expr::Name(target_name) = &*ann_assign.target {
                    self.variables.insert(
                        target_name.id.to_string(),
                        (schema_name.id.to_string(), current_line),
                    );
                    *schema_resolved = true;
                }
            }
        }
    }

    fn annotated_assign_method_call(
        &mut self,
        ann_assign: &ruff_python_ast::StmtAnnAssign,
        current_line: usize,
        schema_resolved: &mut bool,
        attr: &ruff_python_ast::ExprAttribute,
    ) {
        let current_expr = &*attr.value;
        if let Expr::Call(inner_call) = current_expr {
            self.annotated_assign_schema_instance(
                ann_assign,
                current_line,
                schema_resolved,
                inner_call,
            );
        }
    }

    fn annotated_assign_schema_instance(
        &mut self,
        ann_assign: &ruff_python_ast::StmtAnnAssign,
        current_line: usize,
        schema_resolved: &mut bool,
        inner_call: &ruff_python_ast::ExprCall,
    ) {
        if let Expr::Name(schema_name) = &*inner_call.func {
            if self.schemas.contains_key(schema_name.id.as_str()) {
                if let Expr::Name(target_name) = &*ann_assign.target {
                    self.variables.insert(
                        target_name.id.to_string(),
                        (schema_name.id.to_string(), current_line),
                    );
                    *schema_resolved = true;
                }
            }
        }
    }

    fn track_annotation_schema(
        &mut self,
        ann_assign: &ruff_python_ast::StmtAnnAssign,
        current_line: usize,
        schema_resolved: &mut bool,
    ) {
        match &*ann_assign.annotation {
            Expr::Subscript(subscript) => {
                self.track_subscript_annotation(
                    ann_assign,
                    current_line,
                    schema_resolved,
                    subscript,
                );
            }
            // Handle quoted type hints: df: "DataFrame[UserSchema]"
            Expr::StringLiteral(s)
                if self.parse_quoted_type_hint(s.value.to_str(), ann_assign, current_line) =>
            {
                *schema_resolved = true;
            }
            _ => {}
        }
    }

    fn track_subscript_annotation(
        &mut self,
        ann_assign: &ruff_python_ast::StmtAnnAssign,
        current_line: usize,
        schema_resolved: &mut bool,
        subscript: &ruff_python_ast::ExprSubscript,
    ) {
        let mut type_name = None;
        if let Expr::Name(name) = &*subscript.value {
            type_name = Some(name.id.as_str());
        } else if let Expr::Attribute(attr) = &*subscript.value {
            type_name = Some(attr.attr.as_str());
        }

        if let Some(name) = type_name {
            // DataFrame[Schema]
            self.track_dataframe_annotation(
                ann_assign,
                current_line,
                schema_resolved,
                subscript,
                name,
            );
        }
    }

    fn track_dataframe_annotation(
        &mut self,
        ann_assign: &ruff_python_ast::StmtAnnAssign,
        current_line: usize,
        schema_resolved: &mut bool,
        subscript: &ruff_python_ast::ExprSubscript,
        name: &str,
    ) {
        if name == "DataFrame" {
            if let Expr::Name(schema_name) = &*subscript.slice {
                if let Expr::Name(target_name) = &*ann_assign.target {
                    self.variables.insert(
                        target_name.id.to_string(),
                        (schema_name.id.to_string(), current_line),
                    );
                    *schema_resolved = true;
                }
            }
        } else if name == "Annotated" {
            // Annotated[DataFrame, Schema] or Annotated[pl.DataFrame, Schema]
            self.track_annotated_dataframe(ann_assign, current_line, schema_resolved, subscript);
        }
    }

    fn track_annotated_dataframe(
        &mut self,
        ann_assign: &ruff_python_ast::StmtAnnAssign,
        current_line: usize,
        schema_resolved: &mut bool,
        subscript: &ruff_python_ast::ExprSubscript,
    ) {
        if let Expr::Tuple(tuple) = &*subscript.slice {
            self.track_tuple_annotation(ann_assign, current_line, schema_resolved, tuple);
        }
    }

    fn track_tuple_annotation(
        &mut self,
        ann_assign: &ruff_python_ast::StmtAnnAssign,
        current_line: usize,
        schema_resolved: &mut bool,
        tuple: &ruff_python_ast::ExprTuple,
    ) {
        if tuple.elts.len() >= 2 {
            self.track_tuple_annotation_elements(ann_assign, current_line, schema_resolved, tuple);
        }
    }

    fn track_tuple_annotation_elements(
        &mut self,
        ann_assign: &ruff_python_ast::StmtAnnAssign,
        current_line: usize,
        schema_resolved: &mut bool,
        tuple: &ruff_python_ast::ExprTuple,
    ) {
        let mut is_dataframe = false;
        if let Expr::Name(first) = &tuple.elts[0] {
            let first_name = first.id.as_str();
            if first_name == "DataFrame" || first_name.contains("DataFrame") {
                is_dataframe = true;
            }
        } else if let Expr::Attribute(first_attr) = &tuple.elts[0] {
            if first_attr.attr.as_str() == "DataFrame" {
                is_dataframe = true;
            }
        }
        if is_dataframe {
            if let Expr::Name(schema_name) = &tuple.elts[1] {
                if let Expr::Name(target_name) = &*ann_assign.target {
                    self.variables.insert(
                        target_name.id.to_string(),
                        (schema_name.id.to_string(), current_line),
                    );
                    *schema_resolved = true;
                }
            }
        }
    }

    // Returns whether a schema was actually resolved and attached to the target
    // variable, so the caller (AnnAssign handling) can count it toward DataFrame
    // schema coverage exactly once.
    fn parse_quoted_type_hint(
        &mut self,
        s: &str,
        ann_assign: &ast::StmtAnnAssign,
        current_line: usize,
    ) -> bool {
        // Handle patterns like "DataFrame[Schema]"
        // and "Annotated[DataFrame, Schema]", "Annotated[pl.DataFrame, Schema]"

        if s.contains("DataFrame[") {
            if let Some(value) = self.quoted_type_mentions_dataframe(s, ann_assign, current_line) {
                return value;
            }
            return false;
        }

        // Handle Annotated pattern
        if s.contains("Annotated[") && s.contains("DataFrame") {
            // Extract schema from Annotated[DataFrame, Schema] or Annotated[pl.DataFrame, Schema]
            if let Some(value) = self.quoted_annotated_schema(s, ann_assign, current_line) {
                return value;
            }
        }
        false
    }

    fn quoted_type_mentions_dataframe(
        &mut self,
        s: &str,
        ann_assign: &ruff_python_ast::StmtAnnAssign,
        current_line: usize,
    ) -> Option<bool> {
        if let Some(start) = s.find('[') {
            if let Some(end) = s.rfind(']') {
                let schema_name = &s[start + 1..end];
                // Handle nested generics by taking the last part
                let schema = schema_name
                    .split(',')
                    .next_back()
                    .unwrap_or(schema_name)
                    .trim();
                if let Expr::Name(target_name) = &*ann_assign.target {
                    self.variables.insert(
                        target_name.id.to_string(),
                        (schema.to_string(), current_line),
                    );
                    return Some(true);
                }
            }
        }
        None
    }

    fn quoted_annotated_schema(
        &mut self,
        s: &str,
        ann_assign: &ruff_python_ast::StmtAnnAssign,
        current_line: usize,
    ) -> Option<bool> {
        if let Some(start) = s.find("Annotated[") {
            if let Some(value) = self.quoted_annotated_start(s, ann_assign, current_line, start) {
                return value;
            }
        }
        None
    }

    fn quoted_annotated_start(
        &mut self,
        s: &str,
        ann_assign: &ruff_python_ast::StmtAnnAssign,
        current_line: usize,
        start: usize,
    ) -> Option<Option<bool>> {
        let inner = &s[start + 10..];
        // Skip "Annotated["
        if let Some(end) = inner.rfind(']') {
            let parts: Vec<&str> = inner[..end].split(',').collect();
            if parts.len() >= 2 {
                let schema = parts[1].trim();
                if let Expr::Name(target_name) = &*ann_assign.target {
                    self.variables.insert(
                        target_name.id.to_string(),
                        (schema.to_string(), current_line),
                    );
                    return Some(Some(true));
                }
            }
        }
        None
    }
}
