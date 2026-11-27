use super::*;

impl Linter {
    pub fn new() -> Self {
        Self::default()
    }

    // Apply project-level context resolved by the caller: the project root (for
    // resolving `.sql` file reads traced back from a load call) and the configured SQL
    // dialect (for identifier case folding — see `sql::SqlDialect`). Kept as a
    // post-construction setter rather than a `new()` parameter so the 35+ existing
    // `Linter::new()` call sites (mostly tests, which don't care about either) are
    // undisturbed.
    pub fn with_context(&mut self, root: PathBuf, config: &LinterConfig) {
        self.project_root = Some(root);
        if let Some(dialect) = &config.sql_dialect {
            self.sql_dialect = sql::SqlDialect::from_config_str(dialect);
        }
    }

    // Convert a byte offset to a 1-indexed (line, column) pair using the pre-built
    // LineIndex (O(log n) binary search).  Values come from OneIndexed::get() so they
    // are already 1-based — no adjustment needed at call sites.
    pub(super) fn source_location(&self, offset: ruff_text_size::TextSize) -> (usize, usize) {
        let source_code = SourceCode::new(
            &self.source,
            self.line_index
                .as_ref()
                .expect("LineIndex should be initialized before calling source_location"),
        );
        let loc = source_code.line_column(offset);
        (loc.line.get(), loc.column.get())
    }

    // Format a schema name for use in an error message. Always includes the full
    // column set, so an "unknown column" message tells the reader what IS available
    // without a separate lookup — for inferred schemas (those whose name starts with
    // `__inferred_`), also includes the origin function/file when recorded by
    // load_cross_file_symbols (where the columns were actually derived from); for
    // named schemas (BaseSchema subclasses), the schema's own class-definition
    // file/line from `schema_locations` — which may be a different file entirely than
    // wherever the erroring variable was bound, so `defined_line` (that binding's own
    // line, in the CURRENT file) is only a fallback for the rare case a named schema
    // has no recorded location at all.
    pub(super) fn schema_display(&self, schema_name: &str, defined_line: usize) -> String {
        let cols = self.schemas.get(schema_name).cloned().unwrap_or_default();
        let cols_str = cols.join(", ");
        if schema_name.starts_with("__inferred_") {
            if let Some(origin) = self.schema_origins.get(schema_name) {
                format!(
                    "inferred column set {{{cols_str}}} — fix: add the column at its source in {origin}"
                )
            } else {
                format!("inferred column set {{{cols_str}}} (defined at line {defined_line})")
            }
        } else if let Some((file, line)) = self.schema_locations.get(schema_name) {
            format!("{schema_name} {{{cols_str}}} (defined at {file}:{line})")
        } else {
            format!("{schema_name} {{{cols_str}}} (defined at line {defined_line})")
        }
    }

    // Parse `source`, walk the AST, then filter out any diagnostic whose line carries a
    // `# typedframes: ignore` comment.  Returns the surviving errors.
    pub fn check_file_internal(
        &mut self,
        source: &str,
        path: &Path,
    ) -> Result<Vec<LintError>, anyhow::Error> {
        let parsed = parse_module(source).map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(self.check_parsed_module(parsed.into_syntax(), source, path))
    }

    // Same as `check_file_internal`, but parses in Jupyter/IPython mode so magics
    // (`%foo`) and shell escapes (`!foo`) tokenize as `Stmt::IpyEscapeCommand` nodes
    // instead of causing a syntax error -- the AST walk below simply doesn't match
    // that variant, so it's a no-op for schema/column tracking. `source` is expected
    // to be a notebook's concatenated code-cell source (see `notebook.rs` and
    // `ruff_notebook::Notebook::source_code`), not a `.ipynb` file's raw JSON.
    pub fn check_notebook_internal(
        &mut self,
        source: &str,
        path: &Path,
    ) -> Result<Vec<LintError>, anyhow::Error> {
        let parsed =
            parse(source, ParseOptions::from(Mode::Ipython)).map_err(|e| anyhow::anyhow!("{e}"))?;
        let module = parsed
            .try_into_module()
            .expect("Mode::Ipython always parses to a ModModule")
            .into_syntax();
        Ok(self.check_parsed_module(module, source, path))
    }

    // Shared by `check_file_internal` and `check_notebook_internal`: everything
    // after parsing succeeds -- schema/variable tracking via the AST walk, then
    // filtering out any diagnostic whose line carries a `# typedframes: ignore`
    // comment.
    pub(crate) fn check_parsed_module(
        &mut self,
        mut module: ModModule,
        source: &str,
        path: &Path,
    ) -> Vec<LintError> {
        // Every name or expression that can be evaluated without running the program is
        // replaced by the literal it equals (source ranges kept), so everything below sees
        // a literal where there was a variable -- see `static_eval`.
        let (folds, reads) = {
            let analysis = static_eval::Analysis::build(&module, &self.imported_modules);
            self.missing_enum_members = analysis.missing_enum_members(&module);
            self.contract_targets = contract_targets(&analysis, &module);
            self.contract_targets_ready = true;
            static_eval::plan_folds(&module, &analysis)
        };
        static_eval::apply_folds(&mut module, &folds);
        self.lint_folded(&module, reads, source, path)
    }

    // Everything after folding: `module` already has its constants replaced, and `reads` are
    // the class constants read through `self`/`cls` that went into that.
    pub(crate) fn lint_folded(
        &mut self,
        module: &ModModule,
        self_constant_reads: Vec<static_eval::SelfConstantRead>,
        source: &str,
        path: &Path,
    ) -> Vec<LintError> {
        let lifted = crate::returned_loads::lift(module);
        let module = lifted.as_ref().unwrap_or(module);
        let dicts = crate::dict_frames::rewrite(module);
        let module = dicts.as_ref().unwrap_or(module);
        match crate::self_frames::rewrite(module) {
            Some((rewritten, frames)) => {
                self.method_frames = frames;
                self.lint_lifted(&rewritten, self_constant_reads, source, path)
            }
            None => {
                self.method_frames.clear();
                self.lint_lifted(module, self_constant_reads, source, path)
            }
        }
    }

    fn lint_lifted(
        &mut self,
        module: &ModModule,
        self_constant_reads: Vec<static_eval::SelfConstantRead>,
        source: &str,
        path: &Path,
    ) -> Vec<LintError> {
        self.source = source.to_string();
        self.file_display = path.display().to_string();
        self.line_index = Some(LineIndex::from_source_text(source));
        self.collect_value_candidates(module, path);

        let mut errors = Vec::new();

        for stmt in &module.body {
            self.visit_stmt(stmt, &mut errors);
        }
        self.report_missing_enum_members(module, self_constant_reads, source, &mut errors);

        self.observe_untyped_site_usage(module);
        self.name_untracked_sites();
        self.report_likely_typos(module, &mut errors);

        let lines: Vec<&str> = source.lines().collect();
        errors.retain(|e| !is_line_ignored_in(&lines, e.line, &e.code));
        keep_most_helpful_unknown_column(&mut errors);
        let dropped = repeats_of_nested_column_sets(&errors);
        self.finalise_errors(module, errors, dropped)
    }

    fn finalise_errors(
        &mut self,
        module: &ModModule,
        mut errors: Vec<LintError>,
        dropped: Vec<bool>,
    ) -> Vec<LintError> {
        let mut index = 0;
        errors.retain(|_| {
            index += 1;
            !dropped[index - 1]
        });
        // A test should see the whole frame, so narrowing advice there contradicts `suggest`.
        let tests: Vec<(usize, usize)> = module
            .body
            .iter()
            .flat_map(|stmt| match stmt {
                Stmt::FunctionDef(f) => vec![f],
                Stmt::ClassDef(c) => c
                    .body
                    .iter()
                    .filter_map(|s| match s {
                        Stmt::FunctionDef(f) => Some(f),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            })
            .filter(|f| f.name.as_str() == "test" || f.name.as_str().starts_with("test_"))
            .map(|f| {
                (
                    self.source_location(f.range.start()).0,
                    self.source_location(f.range.end()).0,
                )
            })
            .collect();
        for e in errors.iter_mut().filter(|e| {
            e.code == CODE_UNTRACKED_DATAFRAME
                && e.message.contains("specify `usecols`/`columns`")
                && tests
                    .iter()
                    .any(|(start, end)| (*start..=*end).contains(&e.line))
        }) {
            e.message = "columns unknown at lint time; in a test, annotate the variable's type \
                         to check the columns read, e.g. `df: Annotated[pd.DataFrame, MySchema] = \
                         pd.read_csv(...)`"
                .to_string();
        }
        // A test's own column reads are the run-time check; flagging them as unverifiable is
        // noise.
        errors.retain(|e| {
            !(e.code == CODE_UNVERIFIABLE_COLUMN
                && tests
                    .iter()
                    .any(|(start, end)| (*start..=*end).contains(&e.line)))
        });
        collapse_repeated_untracked_warnings(&mut errors);

        errors
    }
}
