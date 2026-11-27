use super::*;

impl Linter {
    pub(super) fn report_missing_enum_members(
        &mut self,
        module: &ModModule,
        self_constant_reads: Vec<static_eval::SelfConstantRead>,
        source: &str,
        errors: &mut Vec<LintError>,
    ) {
        for missing in std::mem::take(&mut self.missing_enum_members) {
            let (line, col) = self.source_location(missing.range.start());
            let mut message = format!(
                "`{}` has no member '{}', so this raises AttributeError",
                missing.class, missing.attr
            );
            if let Some(suggestion) = find_best_match(&missing.attr, &missing.members) {
                message.push_str(&format!(" (did you mean '{suggestion}'?)"));
            }
            errors.push(LintError {
                line,
                col,
                code: CODE_UNKNOWN_COLUMN.to_string(),
                message,
                severity: "error".to_string(),
            });
        }

        // A class constant read through `self` is trusted unless a subclass elsewhere
        // rebinds it, which can't be seen from here; `Final` rules that out.
        let mut advised: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        // A string compared against a cell value (`df["route"] == self.IV`) is data, not a column
        // name, and nothing in a column check depends on it staying fixed.
        let compared = compared_ranges(module);
        for read in self_constant_reads
            .iter()
            .filter(|r| !r.is_final && !compared.iter().any(|c| c.contains_range(r.range)))
        {
            if advised.insert((read.class.clone(), read.attr.clone())) {
                let (line, col) = self.report_self_constant_read(module, source, read);
                errors.push(LintError {
                    line,
                    col,
                    code: CODE_ASSUMED_CONSTANT.to_string(),
                    message: format!(
                        "`self.{attr}` is read as a constant, assuming no subclass in another \
                         file overrides it; declare `{attr}: Final = ...` to make that certain",
                        attr = read.attr
                    ),
                    severity: "info".to_string(),
                });
            }
        }

        // Resolve every pending pass-through-count reversal now that the whole file
        // has been walked and `functions_called` is complete -- see
        // `pending_passthrough_reversals`'s doc comment for why this can't happen
        // eagerly, at the point each candidate was discovered.
        for (func_name, reversal) in &self.pending_passthrough_reversals {
            if !self.functions_called.contains(func_name) {
                continue;
            }
            if let Some(pos) = self
                .typed_sites
                .iter()
                .position(|s| s.line == reversal.line && s.var == reversal.var)
            {
                self.typed_sites.remove(pos);
                self.dataframes_typed -= 1;
                self.dataframes_total -= 1;
            } else if let Some(pos) = self
                .untyped_sites
                .iter()
                .position(|s| s.line == reversal.line && s.var == reversal.var)
            {
                self.untyped_sites.remove(pos);
                self.dataframes_total -= 1;
            }
        }
    }

    pub(super) fn collect_value_candidates(&mut self, module: &ModModule, path: &Path) {
        (self.string_var_candidates, self.bool_var_candidates) =
            self.collect_literal_var_candidates(&module.body, path);
        self.dict_list_var_candidates = collect_row_var_candidates(&module.body);
        self.forwarders = module
            .body
            .iter()
            .filter_map(|stmt| match stmt {
                Stmt::FunctionDef(func_def) => {
                    forwarder_shape(func_def).map(|shape| (func_def.name.to_string(), shape))
                }
                _ => None,
            })
            .collect();
        // Registered ahead of the walk so a function can annotate with a schema defined below it.
        // The walk registers each class again and reports its diagnostics, so these are dropped.
        let mut registration_errors = Vec::new();
        for stmt in &module.body {
            if let Stmt::ClassDef(class_def) = stmt {
                self.register_schema_class(class_def, &mut registration_errors);
            }
        }
        let mut df_usage_collector = DataFrameShapedUsageCollector {
            names: std::collections::HashSet::new(),
        };
        for stmt in &module.body {
            df_usage_collector.visit_stmt(stmt);
        }
        self.dataframe_shaped_usage = df_usage_collector.names;
        self.polars_only_file = imports_only_polars(&module.body);
        self.polars_names = crate::polars_names::polars_names(&module.body);
        let mut function_names = FunctionNameCollector {
            names: std::collections::HashSet::new(),
            summaries: HashMap::new(),
            seen: std::collections::HashSet::new(),
            current_class: None,
            bases: HashMap::new(),
            annotated_methods: HashMap::new(),
        };
        for stmt in &module.body {
            function_names.visit_stmt(stmt);
        }
        self.file_function_names = function_names.names;
        self.class_bases = function_names.bases;
        for (class, methods) in function_names.annotated_methods {
            self.class_methods.insert(class, methods);
        }
        self.helper_summaries = function_names.summaries;
        for (key, summary) in &self.imported_summaries {
            self.helper_summaries
                .entry(key.clone())
                .or_insert_with(|| summary.clone());
        }
        resolve_delegates(
            &mut self.helper_summaries,
            &self.class_bases,
            self.preserve_unresolved_delegates,
        );
        self.prepare_contracts(module);

        self.all_dataframe_calls = {
            let mut load_call_collector = LoadCallSiteCollector {
                linter: &*self,
                sites: Vec::new(),
                current_context: CallContext::Other,
            };
            for stmt in &module.body {
                load_call_collector.visit_stmt(stmt);
            }
            load_call_collector.sites
        };
    }

    fn report_self_constant_read(
        &mut self,
        module: &ModModule,
        source: &str,
        read: &static_eval::SelfConstantRead,
    ) -> (usize, usize) {
        let (line, col) = self.source_location(read.range.start());
        if self.collect_suggestions {
            match suggest::suggest_final_constant(
                module,
                source,
                &self.suggest_display_path,
                &read.class,
                &read.attr,
            ) {
                Ok(suggestion) => {
                    // Two classes reading the same inherited constant share one edit.
                    let known = self.suggestions.iter().any(|s| {
                        s.kind == suggestion.kind
                            && (s.line, s.col) == (suggestion.line, suggestion.col)
                    });
                    if !known {
                        self.suggestions.push(suggestion);
                    }
                }
                Err(reason) => self.withheld.push(suggest::Withheld {
                    phrase: "assumed-constant".to_string(),
                    line,
                    col,
                    var: read.attr.clone(),
                    reason,
                    cell: None,
                }),
            }
        }
        (line, col)
    }

    // A column read off a frame of unknown columns that the file names nowhere else, one slip
    // from exactly one name the file or the project modules it imports do mention.
    pub(super) fn report_likely_typos(&self, module: &ModModule, errors: &mut Vec<LintError>) {
        let reads = std::mem::take(&mut *self.unknown_frame_reads.borrow_mut());
        if reads.is_empty() {
            return;
        }
        let mut counts: HashMap<String, usize> = HashMap::new();
        let mut mentions = NameMentions {
            counts: &mut counts,
            identifiers: true,
        };
        for stmt in &module.body {
            mentions.visit_stmt(stmt);
        }
        let mut imported = std::collections::HashSet::new();
        for exports in self.imported_modules.values() {
            for value in exports.constants.values() {
                collect_value_strings(value, &mut imported);
            }
            for members in exports.enums.values() {
                imported.extend(members.iter().cloned());
            }
        }
        let mut literals: HashMap<String, usize> = HashMap::new();
        let mut named = NameMentions {
            counts: &mut literals,
            identifiers: false,
        };
        for stmt in &module.body {
            named.visit_stmt(stmt);
        }
        let mut evidence =
            crate::typo::Evidence::new(literals.keys().chain(imported.iter()).cloned());
        let mut reported = std::collections::HashSet::new();
        let mut found = Vec::new();
        // Comparing every read with every name is quadratic over a very large file; past the
        // budget the rest is left unchecked.
        let mut work = TYPO_WORK_BUDGET;
        for (line, col, column) in reads {
            if work == 0 {
                break;
            }
            if counts.get(&column).copied().unwrap_or(0) != 1
                || imported.contains(&column)
                || !reported.insert((line, col))
            {
                continue;
            }
            if let Some(intended) = evidence.typo(&column, &mut work) {
                found.push((line, col, column, intended));
            }
        }
        // Two names read once each, each the other's nearest: nothing says which is the slip.
        let pairs: std::collections::HashSet<(String, String)> = found
            .iter()
            .map(|(_, _, c, i)| (c.clone(), i.clone()))
            .collect();
        for (line, col, column, intended) in found {
            if !pairs.contains(&(intended.clone(), column.clone())) {
                errors.push(LintError {
                    line,
                    col,
                    code: CODE_LIKELY_TYPO.to_string(),
                    message: format!(
                        "Column '{column}' is read here and named nowhere else, but the code does name '{intended}' (did you mean '{intended}'?); the frame's columns are unknown, so this is a guess"
                    ),
                    severity: "warning".to_string(),
                });
            }
        }
    }
}

// How often each name is mentioned: string literals (whole, and the words of one that is not
// a plain name, such as SQL) and keyword names; with `identifiers`, also names, attributes and
// parameters, which say a name is in use but not that it is a column.
// Character pairs `report_likely_typos` may compare in one file (well under 0.1s of work).
const TYPO_WORK_BUDGET: usize = 20_000_000;

struct NameMentions<'m> {
    pub(super) counts: &'m mut HashMap<String, usize>,
    pub(super) identifiers: bool,
}

impl NameMentions<'_> {
    pub(super) fn add(&mut self, name: &str) {
        *self.counts.entry(name.to_string()).or_insert(0) += 1;
    }
}

impl<'a> Visitor<'a> for NameMentions<'_> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        if let (Stmt::FunctionDef(func), true) = (stmt, self.identifiers) {
            for p in func
                .parameters
                .posonlyargs
                .iter()
                .chain(&func.parameters.args)
                .chain(&func.parameters.kwonlyargs)
            {
                self.add(p.parameter.name.as_str());
            }
        }
        ast_visitor::walk_stmt(self, stmt);
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::StringLiteral(lit) => {
                let text = lit.value.to_str();
                if text
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == ' ')
                {
                    self.add(text);
                } else {
                    for word in text
                        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                        .filter(|w| !w.is_empty())
                    {
                        self.add(word);
                    }
                }
            }
            Expr::Name(name) if self.identifiers => self.add(name.id.as_str()),
            Expr::Attribute(attr) if self.identifiers => self.add(attr.attr.as_str()),
            Expr::Call(call) => {
                for kw in &call.arguments.keywords {
                    if let Some(arg) = &kw.arg {
                        self.add(arg.as_str());
                    }
                }
            }
            _ => {}
        }
        ast_visitor::walk_expr(self, expr);
    }
}

fn collect_value_strings(value: &static_eval::Value, out: &mut std::collections::HashSet<String>) {
    match value {
        static_eval::Value::Str(s) => {
            out.insert(s.clone());
        }
        static_eval::Value::List(items)
        | static_eval::Value::Tuple(items)
        | static_eval::Value::Set(items) => {
            for item in items {
                collect_value_strings(item, out);
            }
        }
        static_eval::Value::Dict(pairs) => {
            for (k, v) in pairs {
                collect_value_strings(k, out);
                collect_value_strings(v, out);
            }
        }
        _ => {}
    }
}
