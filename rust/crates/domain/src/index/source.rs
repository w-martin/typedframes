use super::*;

/// The index entry for `source` (the contents of `path`), the external-package candidates
/// it names, and whether the result depends on nothing but `source` -- `false` when
/// indexing read another file (a `.sql` file), so a cached copy could go stale without
/// this file changing.
pub(crate) fn index_source(
    fs: &dyn crate::ports::FileSystem,
    path: &Path,
    source: &str,
    project_root: &Path,
    config: &LinterConfig,
) -> Option<(IndexEntry, Vec<String>, bool)> {
    let mut linter = Linter::new();
    linter.fs = fs.handle().into();
    linter.with_context(project_root.to_path_buf(), config);
    linter.preserve_unresolved_delegates = true;
    // Parsed and analysed once. Everything that reads the module as written -- its exports,
    // helper summaries, return libraries and imports -- is read first, because constant
    // folding then rewrites the module in place for the lint. Nothing here has imports to
    // resolve.
    let parsed_module = ruff_python_parser::parse_module(source).ok();
    let WrittenModule {
        eval,
        usages,
        libraries,
        exports,
        imports,
        module_aliases,
        call_sites,
        targets,
        foreign_writes,
        folds,
    } = read_as_written(parsed_module.as_ref(), source, &mut linter);
    let mut folded_and_checked = false;
    if let (Some(parsed), Some((folds, reads))) = (parsed_module, folds) {
        let mut module = parsed.into_syntax();
        crate::static_eval::apply_folds(&mut module, &folds);
        let _ = linter.lint_folded(&module, reads, source, path);
        folded_and_checked = true;
    }
    if !folded_and_checked {
        let _ = linter.check_file_internal(source, path);
    }
    let depends_on_other_files = linter.read_external_file.get();

    // Union function names across the return-schema, requires, delegates, and
    // param-schema-name maps — a function may appear in any subset of the four.
    let mut functions = index_functions(&linter);
    let class_method_libraries = function_libraries(usages, libraries, &mut functions);

    let external_package_candidates =
        external_package_candidates(fs, project_root, &linter, &imports, &module_aliases);
    let class_helper_summaries =
        class_helper_summaries(std::mem::take(&mut linter.helper_summaries));
    let local_contracts = std::mem::take(&mut linter.local_contracts);
    let (contracts, contract_forwards) = indexed_contracts(&linter, local_contracts);
    let mut protocol_classes: Vec<String> = linter.protocol_classes.iter().cloned().collect();
    protocol_classes.sort();
    let class_method_names = sorted_class_method_names(linter.class_method_names);
    let schemas = linter.schemas;
    let schema_locations = linter.schema_locations;
    Some((
        IndexEntry {
            schemas,
            schema_locations,
            functions,
            exports,
            imports,
            module_aliases,
            class_methods: linter.class_methods,
            class_method_libraries,
            class_bases: linter.class_bases,
            class_helper_summaries,
            eval,
            foreign_writes,
            import_targets: targets,
            call_sites,
            contracts,
            contract_forwards,
            class_method_names,
            protocol_classes,
        },
        external_package_candidates,
        !depends_on_other_files,
    ))
}

// What indexing reads from a module as written, before constants are folded into it.
#[derive(Default)]
struct WrittenModule {
    pub(super) eval: crate::static_eval::ModuleExports,
    pub(super) usages: HashMap<String, Vec<crate::column_usage::ParamUsage>>,
    pub(super) libraries: (
        HashMap<String, crate::suggest::Library>,
        HashMap<(String, String), crate::suggest::Library>,
    ),
    pub(super) exports: Vec<String>,
    pub(super) imports: HashMap<String, String>,
    pub(super) module_aliases: HashMap<String, String>,
    pub(super) call_sites: CallSites,
    pub(super) targets: Vec<(u32, String)>,
    pub(super) foreign_writes: Vec<crate::static_eval::ForeignWrite>,
    pub(super) folds: Option<(
        crate::static_eval::Folds,
        Vec<crate::static_eval::SelfConstantRead>,
    )>,
}

// The module's exports, parameter usages, libraries, names and call sites as written, and the
// folds to apply to it; empty for a module that doesn't parse. Readies `linter`'s contract
// targets on the way.
fn read_as_written(
    parsed: Option<&ruff_python_parser::Parsed<ast::ModModule>>,
    source: &str,
    linter: &mut Linter,
) -> WrittenModule {
    let Some(parsed) = parsed else {
        return WrittenModule::default();
    };
    let no_imports = crate::static_eval::ImportedModules::new();
    let module = parsed.syntax();
    let targets = import_targets(module);
    let analysis = crate::static_eval::Analysis::build(module, &no_imports);
    let eval = crate::static_eval::export_module(&analysis, source);
    let foreign_writes = analysis.foreign_writes.clone();
    analysis.reset();
    linter.contract_targets = crate::linter::contract_targets(&analysis, module);
    linter.contract_targets_ready = true;
    let usages = crate::helpers::module_param_usages_in(&analysis, module);
    analysis.reset();
    let folds = crate::static_eval::plan_folds(module, &analysis);
    let libraries = (
        crate::suggest::function_libraries(module),
        crate::suggest::class_method_libraries_of(module),
    );
    let (exports, imports, module_aliases) = module_exports_and_imports(module);
    WrittenModule {
        eval,
        usages,
        libraries,
        exports,
        imports,
        module_aliases,
        call_sites: module_call_sites(module),
        targets,
        foreign_writes,
        folds: Some(folds),
    }
}

// Each class's method names, sorted, so the index is the same from run to run.
fn sorted_class_method_names(
    names: HashMap<String, std::collections::HashSet<String>>,
) -> HashMap<String, Vec<String>> {
    names
        .into_iter()
        .map(|(class, methods)| {
            let mut methods: Vec<String> = methods.into_iter().collect();
            methods.sort();
            (class, methods)
        })
        .collect()
}

// The top-level packages of the modules unresolved calls and `self.<attr>` receivers were
// imported from: the external packages worth indexing for this file.
fn external_package_candidates(
    fs: &dyn FileSystem,
    project_root: &Path,
    linter: &Linter,
    imports: &HashMap<String, String>,
    module_aliases: &HashMap<String, String>,
) -> Vec<String> {
    // Resolve a recorded unresolved-call name (an attribute-call receiver or a
    // bare-call callee) to the module it was imported from, then take that module's
    // top-level package name -- matching the same dotted-import-binds-first-segment
    // convention as the `Stmt::Import` handling above. A name that isn't a plain
    // import at all (a local variable, a class instance, a closure) simply has no
    // entry in `imports`/`module_aliases` and is silently dropped here, same as any
    // other unresolvable name elsewhere in this module.
    let resolve_top_level_package = |name: &str, is_attribute_call: bool| -> Option<String> {
        let module_name = if is_attribute_call {
            module_aliases.get(name)
        } else {
            imports.get(name)
        }?;
        module_name.split('.').next().map(str::to_string)
    };

    let mut external_package_candidates: Vec<String> = linter
        .unresolved_dataframe_shaped_calls
        .iter()
        .filter_map(|call| resolve_top_level_package(&call.name, call.is_attribute_call))
        .collect();

    // self.<attr>.<method>(...) receivers whose class resolves to a py.typed-declared
    // package -- traced immediately, without needing dataframe_shaped_usage's "was
    // the result actually used like a DataFrame" confirmation the way the list above
    // requires (see `package_declares_py_typed`'s doc comment for why a py.typed
    // declaration alone is trust enough). Duplicates against the list above collapse
    // naturally once both feed the same `discovered_candidates` HashSet in
    // `build_index_internal`.
    let typed_receivers = linter
        .all_self_attr_origins
        .values()
        .filter_map(|origin| {
            resolve_top_level_package(&origin.resolve_name, origin.is_attribute_call)
        })
        .filter(|package| package_declares_py_typed(fs, project_root, package));
    external_package_candidates.extend(typed_receivers);
    external_package_candidates
}

// The helper summaries of methods, by class and method name.
pub(super) fn class_helper_summaries(
    helper_summaries: HashMap<FnKey, HelperSummary>,
) -> HashMap<String, HashMap<String, HelperSummary>> {
    let mut class_helper_summaries: HashMap<String, HashMap<String, HelperSummary>> =
        HashMap::new();
    for ((class, method), summary) in helper_summaries {
        if let Some(class) = class {
            class_helper_summaries
                .entry(class)
                .or_default()
                .insert(method, summary);
        }
    }
    class_helper_summaries
}

// The file's own contracts worth indexing (not a protocol's), and the forwards they declare.
fn indexed_contracts(
    linter: &Linter,
    local_contracts: HashMap<FnKey, crate::call_contracts::FuncContract>,
) -> (
    HashMap<String, crate::call_contracts::FuncContract>,
    Vec<(String, crate::call_contracts::Forward)>,
) {
    let mut contracts = HashMap::new();
    let mut contract_forwards = Vec::new();
    for (key, contract) in local_contracts {
        if key
            .0
            .as_ref()
            .is_some_and(|c| linter.protocol_classes.contains(c))
        {
            continue;
        }
        let name = crate::call_contracts::index_key(&key);
        let loose = key.0.is_none()
            && (linter.requires.contains_key(&key.1) || linter.delegates.contains_key(&key.1));
        if contract.has_requirements() || !contract.forwards.is_empty() || loose {
            for forward in &contract.forwards {
                contract_forwards.push((name.clone(), forward.clone()));
            }
            contracts.insert(name, contract);
        }
    }
    (contracts, contract_forwards)
}

fn index_functions(linter: &Linter) -> HashMap<String, IndexFunction> {
    let mut func_names: std::collections::BTreeSet<String> =
        linter.functions.keys().cloned().collect();
    func_names.extend(linter.requires.keys().cloned());
    func_names.extend(linter.delegates.keys().cloned());
    func_names.extend(linter.param_schema_names.keys().cloned());
    func_names.extend(linter.param_governed_templates.keys().cloned());
    func_names.extend(linter.all_function_names.iter().cloned());
    let functions: HashMap<String, IndexFunction> = func_names
        .into_iter()
        .map(|name| {
            let returns_schema = linter.functions.get(&name).cloned().unwrap_or_default();
            let (requires, def_line) = linter
                .requires
                .get(&name)
                .cloned()
                .unwrap_or((Vec::new(), 0));
            let delegates = linter.delegates.get(&name).cloned().unwrap_or_default();
            let (param_schema_name, param_def_line) = linter
                .param_schema_names
                .get(&name)
                .cloned()
                .unwrap_or_default();
            let param_governed = linter.param_governed_templates.get(&name).cloned();
            let helper_summary = linter.helper_summaries.get(&(None, name.clone())).cloned();
            // def_line may only be known via param_schema_names if this function had
            // no direct requires/delegates of its own (see the gate in visit_stmt).
            let def_line = if def_line == 0 {
                param_def_line
            } else {
                def_line
            };
            let straight = linter.straight_line_functions.contains(&name);
            (
                name,
                IndexFunction {
                    returns_schema,
                    returns_library: String::new(),
                    param_usage: Vec::new(),
                    requires,
                    def_line,
                    delegates,
                    param_schema_name,
                    param_governed,
                    straight,
                    requires_certain: false,
                    helper_summary,
                },
            )
        })
        .collect();
    functions
}

fn function_libraries(
    usages: HashMap<String, Vec<crate::column_usage::ParamUsage>>,
    libraries: (
        HashMap<String, crate::suggest::Library>,
        HashMap<(String, String), crate::suggest::Library>,
    ),
    functions: &mut HashMap<String, IndexFunction>,
) -> HashMap<String, HashMap<String, String>> {
    let (libraries, class_libraries) = libraries;
    let mut class_method_libraries: HashMap<String, HashMap<String, String>> = HashMap::new();
    for ((class, method), library) in class_libraries {
        class_method_libraries
            .entry(class)
            .or_default()
            .insert(method, library.as_str().to_string());
    }
    for (name, library) in libraries {
        if let Some(function) = functions.get_mut(&name) {
            function.returns_library = library.as_str().to_string();
        }
    }
    for (name, usage) in usages {
        if let Some(function) = functions.get_mut(&name) {
            function.param_usage = usage;
        }
    }
    class_method_libraries
}
