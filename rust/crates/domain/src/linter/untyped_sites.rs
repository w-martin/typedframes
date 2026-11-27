use super::*;

impl Linter {
    // For each `untyped_sites` entry whose own origin statement sits at the top
    // level of its function/module body, records how the rest of that same scope
    // uses the variable -- see `column_usage`'s module doc comment for exactly what
    // this does and does not attempt. Runs once, after the main walk (and after the
    // pass-through-reversal pass above may have removed some sites), so every
    // remaining site is final and its own body slice is fully known.
    pub(super) fn observe_untyped_site_usage(&mut self, module: &ModModule) {
        if self.untyped_sites.is_empty() {
            return;
        }
        let body = module.body.as_slice();
        self.known_columns = {
            let mut names: Vec<String> = self.schemas.values().flatten().cloned().collect();
            names.sort();
            names.dedup();
            names
        };
        self.name_untyped_sites();
        let by_position: HashMap<(usize, usize), &str> = self
            .untyped_sites
            .iter()
            .map(|site| ((site.line, site.col), site.var.as_str()))
            .collect();
        let mut results = SiteScanResults::default();
        let libraries = suggest::LibraryContext::new(
            body,
            &self.imported_function_libraries,
            &self.imported_class_method_libraries,
        );
        let no_imports = static_eval::ImportedModules::new();
        let analysis = static_eval::Analysis::build(module, &no_imports);
        let helpers =
            helpers::FileHelpers::new(&analysis, &self.imported_param_usage).with_methods(module);
        self.scan_scope_for_untyped_usage(
            &libraries,
            &helpers,
            body,
            &by_position,
            FunctionContext::module(),
            &mut results,
        );
        self.untyped_site_usage = results.usage;
        if self.collect_suggestions {
            self.settle_suggestions(results.outcomes);
        }
    }

    // Libraries of functions this file imports from other project files, by local name.
    pub(crate) fn imported_function_libraries(&self) -> &HashMap<String, suggest::Library> {
        &self.imported_function_libraries
    }

    // Libraries of the methods of classes this file imports from other project files, by
    // `(class, method)`.
    pub(crate) fn imported_class_method_libraries(
        &self,
    ) -> &HashMap<(String, String), suggest::Library> {
        &self.imported_class_method_libraries
    }

    // The file's path relative to the project root, with `/` separators, so a site's name
    // doesn't depend on where the checker was run from. Falls back to the bare file name.
    fn name_salt(&self) -> String {
        let path = Path::new(&self.file_display);
        let relative = self
            .project_root
            .as_deref()
            .and_then(|root| path.strip_prefix(root).ok())
            .or_else(|| path.file_name().map(Path::new))
            .unwrap_or(path);
        relative.to_string_lossy().replace('\\', "/")
    }

    // Gives every frame the checker does not track (see `unresolved_dataframe_shaped_calls`) a
    // two-word name, from a different salt than the untyped sites' so the two never collide.
    pub(super) fn name_untracked_sites(&mut self) {
        let Some(index) = self.line_index.as_ref() else {
            return;
        };
        if !self.collect_suggestions || self.unresolved_dataframe_shaped_calls.is_empty() {
            return;
        }
        let sites: Vec<(usize, usize, &str)> = self
            .unresolved_dataframe_shaped_calls
            .iter()
            .map(|c| (c.line, c.col, c.var.as_str()))
            .collect();
        let salt = format!("untracked:{}", self.name_salt());
        let phrases = crate::phrase::site_phrases(&self.source, index, &salt, &sites);
        for (call, phrase) in self
            .unresolved_dataframe_shaped_calls
            .iter_mut()
            .zip(phrases)
        {
            call.phrase = phrase;
        }
    }

    // Gives every untyped site its stable two-word name; see `phrase`.
    fn name_untyped_sites(&mut self) {
        let Some(index) = self.line_index.as_ref() else {
            return;
        };
        let sites: Vec<(usize, usize, &str)> = self
            .untyped_sites
            .iter()
            .map(|s| (s.line, s.col, s.var.as_str()))
            .collect();
        let phrases = crate::phrase::site_phrases(&self.source, index, &self.name_salt(), &sites);
        for (site, phrase) in self.untyped_sites.iter_mut().zip(phrases) {
            site.phrase = phrase;
        }
    }

    // `scope_body` is a whole function (or module) body; `in_function` says which. Finds
    // every origin in it, however deeply nested in `if`/`with`/`try`/`match` blocks, and
    // every nested function and method, and records how the rest of its scope uses it.
    fn scan_scope_for_untyped_usage(
        &self,
        libraries: &suggest::LibraryContext,
        helpers: &dyn column_usage::HelperResolver,
        scope_body: &[Stmt],
        by_position: &HashMap<(usize, usize), &str>,
        function: FunctionContext,
        results: &mut SiteScanResults,
    ) {
        let scope = UsageScope {
            libraries,
            helpers,
            body: scope_body,
            by_position,
            in_function: function.name.is_some(),
            function_name: function.name,
            receivers: function.receivers,
        };
        self.scan_block(&scope, scope_body, &[], false, results);
    }

    // `block` is a run of statements inside `scope`; `tails` is what runs after `block`
    // finishes, innermost first (the remainder of each enclosing block). An origin's
    // downstream is therefore the rest of its own block followed by those tails.
    fn scan_block<'a>(
        &self,
        scope: &UsageScope,
        block: &'a [Stmt],
        tails: &[&'a [Stmt]],
        in_loop: bool,
        results: &mut SiteScanResults,
    ) {
        for (i, stmt) in block.iter().enumerate() {
            self.scan_origin(scope, stmt, &block[i + 1..], tails, in_loop, results);

            let rest = &block[i + 1..];
            let after = |extra: &[&'a [Stmt]]| following(extra, rest, tails);
            match stmt {
                Stmt::FunctionDef(func_def) => {
                    self.scan_scope_for_untyped_usage(
                        scope.libraries,
                        scope.helpers,
                        &func_def.body,
                        scope.by_position,
                        FunctionContext {
                            name: Some(func_def.name.as_str()),
                            receivers: receiver_classes(func_def),
                        },
                        results,
                    );
                }
                Stmt::ClassDef(class_def) => self.scan_class_body(scope, &class_def.body, results),
                Stmt::If(if_stmt) => {
                    let chain = after(&[]);
                    self.scan_block(scope, &if_stmt.body, &chain, in_loop, results);
                    for clause in &if_stmt.elif_else_clauses {
                        self.scan_block(scope, &clause.body, &chain, in_loop, results);
                    }
                }
                Stmt::With(with_stmt) => {
                    let chain = after(&[]);
                    self.scan_block(scope, &with_stmt.body, &chain, in_loop, results);
                }
                Stmt::Match(match_stmt) => {
                    let chain = after(&[]);
                    for case in &match_stmt.cases {
                        self.scan_block(scope, &case.body, &chain, in_loop, results);
                    }
                }
                Stmt::Try(try_stmt) => {
                    // After an exception the handlers run, then `finally`; after a clean
                    // body, `else` then `finally`.
                    let handlers: Vec<&[Stmt]> = try_stmt
                        .handlers
                        .iter()
                        .map(|h| {
                            let ast::ExceptHandler::ExceptHandler(h) = h;
                            h.body.as_slice()
                        })
                        .collect();
                    let finalbody = try_stmt.finalbody.as_slice();
                    let mut body_extra: Vec<&[Stmt]> = handlers.clone();
                    body_extra.push(try_stmt.orelse.as_slice());
                    body_extra.push(finalbody);
                    let body_chain = after(&body_extra);
                    self.scan_block(scope, &try_stmt.body, &body_chain, in_loop, results);
                    let tail_chain = after(&[finalbody]);
                    for handler in &handlers {
                        self.scan_block(scope, handler, &tail_chain, in_loop, results);
                    }
                    self.scan_block(scope, &try_stmt.orelse, &tail_chain, in_loop, results);
                    self.scan_block(scope, finalbody, &after(&[]), in_loop, results);
                }
                // A statement that runs again can read the frame before the origin on its
                // next pass, which the forward scan never sees.
                Stmt::For(for_stmt) => {
                    let chain = after(&[]);
                    self.scan_block(scope, &for_stmt.body, &chain, true, results);
                    self.scan_block(scope, &for_stmt.orelse, &chain, in_loop, results);
                }
                Stmt::While(while_stmt) => {
                    let chain = after(&[]);
                    self.scan_block(scope, &while_stmt.body, &chain, true, results);
                    self.scan_block(scope, &while_stmt.orelse, &chain, in_loop, results);
                }
                _ => {}
            }
        }
    }

    // Methods (and nested classes' methods) are scopes of their own; an assignment directly
    // in a class body is a class attribute, not a local, so it is never an origin here.
    fn scan_class_body(
        &self,
        scope: &UsageScope,
        class_body: &[Stmt],
        results: &mut SiteScanResults,
    ) {
        // `self.store = store` with `store: Store` in `__init__`: methods can call `self.store`.
        let mut self_attrs = HashMap::new();
        for stmt in class_body {
            if let Stmt::FunctionDef(func_def) = stmt {
                if func_def.name.as_str() == "__init__" {
                    collect_self_attr_origins(
                        &func_def.body,
                        &annotated_param_origins(func_def),
                        &mut self_attrs,
                    );
                }
            }
        }
        for stmt in class_body {
            self.scan_class_member(scope, results, &self_attrs, stmt);
        }
    }

    fn scan_class_member(
        &self,
        scope: &UsageScope<'_>,
        results: &mut SiteScanResults,
        self_attrs: &HashMap<String, SelfAttrOrigin>,
        stmt: &Stmt,
    ) {
        match stmt {
            Stmt::FunctionDef(func_def) => {
                self.scan_scope_for_untyped_usage(
                    scope.libraries,
                    scope.helpers,
                    &func_def.body,
                    scope.by_position,
                    FunctionContext {
                        name: Some(func_def.name.as_str()),
                        receivers: {
                            let mut receivers = receiver_classes(func_def);
                            receivers.extend(
                                self_attrs.iter().map(|(attr, o)| {
                                    (format!("self.{attr}"), o.class_name.clone())
                                }),
                            );
                            receivers
                        },
                    },
                    results,
                );
            }
            Stmt::ClassDef(inner) => self.scan_class_body(scope, &inner.body, results),
            _ => {}
        }
    }
}
