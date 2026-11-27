use super::*;

impl Linter {
    // Load schemas and functions from cross-file index based on import statements.
    pub(crate) fn load_cross_file_symbols(
        &mut self,
        index: &ProjectIndex,
        source: &str,
        file_path: &Path,
        project_root: &Path,
    ) {
        let Ok(parsed) = parse_module(source) else {
            return;
        };
        self.load_cross_file_symbols_from_module(
            index,
            &parsed.into_syntax(),
            project_root,
            file_path.to_str(),
        );
    }

    // Same as `load_cross_file_symbols`, but parses `source` in Jupyter/IPython mode
    // -- a notebook's concatenated code-cell source may contain magics/shell escapes
    // anywhere, which would make the strict-Python `parse_module` above fail on the
    // WHOLE file (silently disabling cross-file symbol loading, not just for the
    // cell containing the magic).
    pub(crate) fn load_cross_file_symbols_notebook(
        &mut self,
        index: &ProjectIndex,
        source: &str,
        project_root: &Path,
    ) {
        let Ok(parsed) = parse(source, ParseOptions::from(Mode::Ipython)) else {
            return;
        };
        let Some(module) = parsed.try_into_module() else {
            return;
        };
        self.load_cross_file_symbols_from_module(index, &module.into_syntax(), project_root, None);
    }

    pub(crate) fn load_cross_file_symbols_from_module(
        &mut self,
        index: &ProjectIndex,
        module: &ModModule,
        project_root: &Path,
        file_path: Option<&str>,
    ) {
        let own_root = self.load_imported_modules(index, module, project_root, file_path);

        // A function's return-type schema (or a parameter's schema annotation) may be
        // defined in a THIRD file — neither the function's own file nor the file
        // importing the function. E.g. schemas.py defines CustomerSchema, loaders.py's
        // load_customers() returns Annotated[pd.DataFrame, CustomerSchema], and
        // pipeline.py imports only load_customers, not CustomerSchema directly. Looking
        // the schema up only in the function's own file's entry (as opposed to
        // project-wide) would silently fail in that case — no error, but no validation
        // either, since self.schemas would never learn CustomerSchema's columns.
        // `index.all_schemas` is this project-wide name -> columns registry — computed
        // ONCE in build_index_internal, not rebuilt here. This function runs once per
        // file checked (see `check_file`), so rebuilding a project-wide map in here
        // would cost O(files) per file, i.e. O(files^2) for a whole-project check.
        let all_schemas = &index.all_schemas;
        let all_schema_locations = &index.all_schema_locations;

        let importer = file_path.map(|p| {
            let p = Path::new(p);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                project_root.join(p)
            }
        });
        let import_stmts = top_level_and_type_checking_stmts(module);
        for stmt in import_stmts.iter().copied() {
            let Stmt::ImportFrom(import_from) = stmt else {
                continue;
            };
            if import_from.level > 0 && import_from.module.is_none() {
                continue;
            }
            let resolved = if import_from.level > 0 {
                let module_name = import_from
                    .module
                    .as_ref()
                    .map(|m| m.id.as_str())
                    .unwrap_or_default();
                importer.as_deref().and_then(|file| {
                    resolve_relative_module(file, import_from.level, module_name, &index.files)
                })
            } else {
                let Some(module_ident) = &import_from.module else {
                    continue;
                };
                let module_name = module_ident.id.as_str();
                if module_name.starts_with("typedframes") {
                    continue;
                }
                // resolve_module_file also tries an explicitly allowlisted external
                // package's site-packages location (see collect_external_package_files),
                // not just project_root/project_root/src.
                resolve_module_file_with_root(
                    &*self.fs,
                    module_name,
                    project_root,
                    importer.as_deref(),
                    &own_root,
                    &index.package_roots,
                    &index.files,
                )
            };
            let Some(resolved_str) = resolved else {
                continue;
            };
            let Some(entry) = index.files.get(&resolved_str) else {
                continue;
            };
            // Use the full resolved path (not just the basename) so error messages
            // contain an openable `file:line` reference regardless of cwd.
            if let ControlFlow::Break(_) = self.load_from_import(
                all_schemas,
                all_schema_locations,
                import_from,
                resolved_str,
                entry,
            ) {
                continue;
            }
        }

        // Plain `import module [as alias]`: everything reachable via `module.name`
        // attribute access. Unlike `from X import name`, there's no explicit name
        // list to narrow to, so pull in every function this module defines — a call
        // site written as `module.func(df)` is resolved by check_call_requirements
        // looking up the bare name `func`, the same key space `from X import func`
        // would have populated (see visit_stmt's Expr::Attribute call-site handling).
        for stmt in import_stmts.iter().copied() {
            if let ControlFlow::Break(_) =
                self.load_imported_names(index, project_root, &own_root, &importer, stmt)
            {
                continue;
            }
        }
    }

    fn load_imported_modules(
        &mut self,
        index: &ProjectIndex,
        module: &ModModule,
        project_root: &Path,
        file_path: Option<&str>,
    ) -> std::cell::OnceCell<PathBuf> {
        self.imported_modules =
            collect_imported_modules(&*self.fs, index, module, project_root, file_path);
        let own_root = std::cell::OnceCell::new();
        let mut class_names = std::collections::HashSet::new();
        collect_class_and_imported_names(&module.body, &mut class_names);
        let own_file = file_path.map(|p| {
            let p = Path::new(p);
            if p.is_absolute() {
                p.to_string_lossy().to_string()
            } else {
                project_root.join(p).to_string_lossy().to_string()
            }
        });
        self.overridden_elsewhere = class_names
            .into_iter()
            .filter_map(|class| {
                let methods: std::collections::HashSet<String> = index
                    .overridden
                    .get(class)?
                    .iter()
                    .filter(|(_, file)| Some(file) != own_file.as_ref())
                    .map(|(method, _)| method.clone())
                    .collect();
                (!methods.is_empty()).then(|| (class.to_string(), methods))
            })
            .collect();
        self.method_agreement = index
            .method_agreement
            .iter()
            .map(|(method, (contract, origin))| {
                (method.clone(), (contract.clone(), origin.clone()))
            })
            .collect();
        // Pre-seed cross-file confirmation for pending pass-through reversals (see
        // `pending_passthrough_reversals`'s doc comment) BEFORE this file's own
        // statement walk runs, so the existing end-of-walk resolution loop in
        // `check_parsed_module` picks up a function called only from ANOTHER file with
        // no change to the reversal mechanism itself. `None` for a notebook (which
        // can never itself be a cross-file call TARGET, so `index.called_functions`
        // would never have an entry keyed by its path anyway -- this is a genuine
        // no-op there, not a gap).
        if let Some(path) = file_path {
            if let Some(names) = index.called_functions.get(path) {
                self.functions_called.extend(names.iter().cloned());
            }
        }
        own_root
    }

    fn load_from_import(
        &mut self,
        all_schemas: &HashMap<String, Vec<String>>,
        all_schema_locations: &HashMap<String, (String, usize)>,
        import_from: &ruff_python_ast::StmtImportFrom,
        resolved_str: String,
        entry: &IndexEntry,
    ) -> ControlFlow<()> {
        if let Some(value) = self.load_resolved_import(
            all_schemas,
            all_schema_locations,
            import_from,
            resolved_str,
            entry,
        ) {
            return value;
        }
        ControlFlow::Continue(())
    }

    fn load_resolved_import(
        &mut self,
        all_schemas: &HashMap<String, Vec<String>>,
        all_schema_locations: &HashMap<String, (String, usize)>,
        import_from: &ruff_python_ast::StmtImportFrom,
        resolved_str: String,
        entry: &IndexEntry,
    ) -> Option<ControlFlow<()>> {
        let file_path_display = resolved_str.clone();
        let is_wildcard = import_from.names.len() == 1 && import_from.names[0].name.as_str() == "*";
        if is_wildcard {
            let names: Vec<String> = if !entry.exports.is_empty() {
                entry.exports.clone()
            } else {
                entry
                    .functions
                    .keys()
                    .chain(entry.schemas.keys())
                    .filter(|n| !n.starts_with('_'))
                    .cloned()
                    .collect()
            };
            for name in &names {
                self.import_name(
                    entry,
                    name,
                    &file_path_display,
                    all_schemas,
                    all_schema_locations,
                );
                self.import_helper_summary(entry, name, None, name);
                self.import_contract(
                    entry,
                    name,
                    call_contracts::IMPORTED_SLOT,
                    name,
                    &file_path_display,
                );
            }
            return Some(ControlFlow::Break(()));
        }
        // `from X import *`: ruff represents the wildcard as a single alias named
        // "*". Expand to the module's declared __all__, or — matching real Python
        // semantics for a module with no __all__ — every public (non-`_`-prefixed)
        // function/schema name, since we can't tell which of those the file
        // actually goes on to use.
        for alias in &import_from.names {
            let name = alias.name.id.as_str();
            self.import_name(
                entry,
                name,
                &file_path_display,
                all_schemas,
                all_schema_locations,
            );
            let local = alias.asname.as_ref().map_or(name, |a| a.id.as_str());
            if local != name {
                if let Some(cols) = self.schemas.get(name).cloned() {
                    self.schemas.insert(local.to_string(), cols);
                }
                if let Some(loc) = self.schema_locations.get(name).cloned() {
                    self.schema_locations.insert(local.to_string(), loc);
                }
            }
            self.import_helper_summary(entry, name, None, local);
            self.import_contract(
                entry,
                name,
                call_contracts::IMPORTED_SLOT,
                local,
                &file_path_display,
            );
        }
        None
    }

    fn load_imported_names(
        &mut self,
        index: &ProjectIndex,
        project_root: &Path,
        own_root: &std::cell::OnceCell<PathBuf>,
        importer: &Option<PathBuf>,
        stmt: &Stmt,
    ) -> ControlFlow<()> {
        let Stmt::Import(import_stmt) = stmt else {
            return ControlFlow::Break(());
        };
        for alias in &import_stmt.names {
            if let ControlFlow::Break(_) =
                self.load_imported_alias(index, project_root, own_root, importer, alias)
            {
                continue;
            }
        }
        ControlFlow::Continue(())
    }

    fn load_imported_alias(
        &mut self,
        index: &ProjectIndex,
        project_root: &Path,
        own_root: &std::cell::OnceCell<PathBuf>,
        importer: &Option<PathBuf>,
        alias: &ruff_python_ast::Alias,
    ) -> ControlFlow<()> {
        let all_schemas = &index.all_schemas;
        let all_schema_locations = &index.all_schema_locations;
        let dotted = alias.name.id.as_str();
        if dotted.starts_with("typedframes") {
            return ControlFlow::Break(());
        }
        let Some(resolved_path) = resolve_module_file_with_root(
            &*self.fs,
            dotted,
            project_root,
            importer.as_deref(),
            own_root,
            &index.package_roots,
            &index.files,
        )
        .map(PathBuf::from) else {
            return ControlFlow::Break(());
        };
        let Some(resolved_str) = resolved_path.to_str() else {
            return ControlFlow::Break(());
        };
        let Some(entry) = index.files.get(resolved_str) else {
            return ControlFlow::Break(());
        };
        let file_path_display = resolved_path.display().to_string();
        let names: Vec<String> = entry.functions.keys().cloned().collect();
        let module_alias = alias
            .asname
            .as_ref()
            .map(|a| a.id.to_string())
            .or_else(|| (!dotted.contains('.')).then(|| dotted.to_string()));
        for name in &names {
            self.import_name(
                entry,
                name,
                &file_path_display,
                all_schemas,
                all_schema_locations,
            );
            if let Some(module_alias) = &module_alias {
                self.import_helper_summary(
                    entry,
                    name,
                    Some(format!("module:{module_alias}")),
                    name,
                );
            }
        }
        if let Some(module_alias) = &module_alias {
            let slot = format!("module:{module_alias}");
            for name in entry.contracts.keys().filter(|k| !k.contains('.')) {
                self.import_contract(entry, name, &slot, name, &file_path_display);
            }
        }
        ControlFlow::Continue(())
    }
}
