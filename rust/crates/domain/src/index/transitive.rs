use super::*;

// Memoised DFS over the delegate graph: a function's fully resolved requirement set
// is its own direct requirements plus the union of every delegate's (transitively)
// resolved requirements. `visiting` guards against infinite recursion on a cycle
// (mutually- or self-delegating functions) — a node caught in a cycle contributes
// only its direct requirements to the cycle, rather than looping forever.
pub(crate) fn resolve_node_requires(
    fs: &dyn crate::ports::FileSystem,
    node: &FuncNode,
    project_root: &Path,
    files: &HashMap<String, IndexEntry>,
    memo: &mut HashMap<FuncNode, Vec<String>>,
    visiting: &mut std::collections::HashSet<FuncNode>,
) -> Vec<String> {
    if let Some(cached) = memo.get(node) {
        return cached.clone();
    }
    if visiting.contains(node) {
        return files
            .get(&node.0)
            .and_then(|e| e.functions.get(&node.1))
            .map(|f| f.requires.clone())
            .unwrap_or_default();
    }
    let Some(func) = files.get(&node.0).and_then(|e| e.functions.get(&node.1)) else {
        return Vec::new();
    };

    visiting.insert(node.clone());
    let mut result = func.requires.clone();
    for delegate_name in &func.delegates {
        if let Some(target) =
            resolve_delegate_target(fs, &node.0, delegate_name, project_root, files)
        {
            let target_reqs =
                resolve_node_requires(fs, &target, project_root, files, memo, visiting);
            for col in target_reqs {
                if !result.contains(&col) {
                    result.push(col);
                }
            }
        }
    }
    visiting.remove(node);

    result.sort();
    result.dedup();
    memo.insert(node.clone(), result.clone());
    result
}

// Resolve every function's `requires` to the transitive union of everything it
// (directly or indirectly) delegates its own parameter to. Runs once, project-wide,
// after every file has been indexed independently — only then do we know, for every
// file, which functions live where and what each one's direct requirements are.
pub(crate) fn resolve_transitive_requires(
    fs: &dyn crate::ports::FileSystem,
    project_root: &Path,
    files: &mut HashMap<String, IndexEntry>,
) {
    let nodes: Vec<FuncNode> = files
        .iter()
        .flat_map(|(file, entry)| {
            entry
                .functions
                .keys()
                .map(move |f| (file.clone(), f.clone()))
        })
        .collect();

    let mut memo: HashMap<FuncNode, Vec<String>> = HashMap::new();
    let mut resolved: Vec<(FuncNode, Vec<String>)> = Vec::new();
    for node in nodes {
        let files_ref: &HashMap<String, IndexEntry> = files;
        let mut visiting = std::collections::HashSet::new();
        let r = resolve_node_requires(fs, &node, project_root, files_ref, &mut memo, &mut visiting);
        resolved.push((node, r));
    }

    let mut certain_memo: HashMap<FuncNode, bool> = HashMap::new();
    let certain: Vec<(FuncNode, bool)> = resolved
        .iter()
        .map(|(node, _)| {
            let mut visiting = std::collections::HashSet::new();
            let c = resolve_node_certain(
                fs,
                node,
                project_root,
                files,
                &mut certain_memo,
                &mut visiting,
            );
            (node.clone(), c)
        })
        .collect();
    for ((file, func), reqs) in resolved {
        if let Some(entry) = files.get_mut(&file) {
            if let Some(f) = entry.functions.get_mut(&func) {
                f.requires = reqs;
            }
        }
    }
    for ((file, func), c) in certain {
        if let Some(f) = files
            .get_mut(&file)
            .and_then(|e| e.functions.get_mut(&func))
        {
            f.requires_certain = c;
        }
    }
}

// Whether a function and everything it delegates to are straight-line (see
// `IndexFunction::straight`); a cycle counts as not.
fn resolve_node_certain(
    fs: &dyn crate::ports::FileSystem,
    node: &FuncNode,
    project_root: &Path,
    files: &HashMap<String, IndexEntry>,
    memo: &mut HashMap<FuncNode, bool>,
    visiting: &mut std::collections::HashSet<FuncNode>,
) -> bool {
    if let Some(&done) = memo.get(node) {
        return done;
    }
    if !visiting.insert(node.clone()) {
        return false;
    }
    let certain = files
        .get(&node.0)
        .and_then(|e| e.functions.get(&node.1))
        .is_some_and(|func| {
            func.straight
                && func.delegates.iter().all(|name| {
                    resolve_delegate_target(fs, &node.0, name, project_root, files).is_none_or(
                        |target| {
                            resolve_node_certain(fs, &target, project_root, files, memo, visiting)
                        },
                    )
                })
        });
    visiting.remove(node);
    memo.insert(node.clone(), certain);
    certain
}

// Folds into each contract what a function of another file requires of a parameter handed
// straight on to it (one level: the callee's own contract as its file computed it), and the
// first parameter's transitive requirement from `resolve_transitive_requires`.
pub(crate) fn resolve_contract_forwards(
    fs: &dyn crate::ports::FileSystem,
    project_root: &Path,
    files: &mut HashMap<String, IndexEntry>,
) {
    use crate::call_contracts::ForwardTarget;
    let mut updates: Vec<(String, String, crate::call_contracts::FuncContract)> = Vec::new();
    for (file, entry) in files.iter() {
        for (key, forward) in &entry.contract_forwards {
            let target = match &forward.target {
                ForwardTarget::Imported(name) => entry
                    .imports
                    .get(name)
                    .and_then(|module| resolve_module_file(fs, module, project_root, files))
                    .map(|target_file| (target_file, name.clone())),
                ForwardTarget::Module(alias, name) => {
                    resolve_module_call_target(fs, file, alias, name, project_root, files)
                }
                _ => None,
            };
            let Some(callee) = target.and_then(|(f, name)| files.get(&f)?.contracts.get(&name))
            else {
                continue;
            };
            let base = updates
                .iter()
                .rposition(|(f, k, _)| f == file && k == key)
                .map(|i| updates[i].2.clone())
                .or_else(|| entry.contracts.get(key).cloned());
            if let Some(mut contract) = base {
                contract.inherit(forward, callee);
                updates.push((file.clone(), key.clone(), contract));
            }
        }
    }
    for entry in files.values_mut() {
        entry.contract_forwards = Vec::new();
    }
    for (file, key, contract) in updates {
        if let Some(slot) = files.get_mut(&file).and_then(|e| e.contracts.get_mut(&key)) {
            *slot = contract;
        }
    }
    // `requires` is keyed by bare name, so it describes a module function only when no
    // method of the file shares the name; and it counts only when no guard can skip a read.
    for entry in files.values_mut() {
        let method_names: std::collections::HashSet<&str> = entry
            .contracts
            .keys()
            .filter_map(|k| k.split_once('.').map(|(_, m)| m))
            .chain(
                entry
                    .class_method_names
                    .values()
                    .flatten()
                    .map(String::as_str),
            )
            .collect();
        let mut merged: Vec<(String, Vec<String>)> = Vec::new();
        for (key, contract) in &entry.contracts {
            if key.contains('.')
                || method_names.contains(key.as_str())
                || contract.loose_param.is_none()
            {
                continue;
            }
            if let Some(function) = entry.functions.get(key) {
                if function.requires_certain && !function.requires.is_empty() {
                    merged.push((key.clone(), function.requires.clone()));
                }
            }
        }
        for (key, requires) in merged {
            if let Some(contract) = entry.contracts.get_mut(&key) {
                if let Some(param) = contract.loose_param {
                    contract.add_required(param, &requires);
                }
            }
        }
        entry.contracts.retain(|_, c| c.has_requirements());
    }
}

// See `ProjectIndex::overridden`. Bases are matched by name across files, so two classes
// sharing a name only make more methods count as overridden.
pub(crate) fn resolve_overridden_methods(
    files: &HashMap<String, IndexEntry>,
) -> HashMap<String, Vec<(String, String)>> {
    let mut bases: HashMap<String, Vec<String>> = HashMap::new();
    for entry in files.values() {
        for (class, parents) in &entry.class_bases {
            bases
                .entry(class.clone())
                .or_default()
                .extend(parents.iter().cloned());
        }
    }
    let mut out: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for (file, entry) in files {
        for (class, methods) in &entry.class_method_names {
            for ancestor in crate::frame_ops::class_chain(&bases, class)
                .into_iter()
                .skip(1)
            {
                let slot = out.entry(ancestor).or_default();
                slot.extend(methods.iter().map(|m| (m.clone(), file.clone())));
            }
        }
    }
    out
}

// For each method name, the requirement shared by every class in the project defining it
// (a class without a contract for it requires nothing), when there is one.
pub(crate) fn resolve_method_agreement(
    files: &HashMap<String, IndexEntry>,
) -> HashMap<String, (crate::call_contracts::FuncContract, String)> {
    let mut seen: HashMap<&str, Option<(&crate::call_contracts::FuncContract, &str)>> =
        HashMap::new();
    // Sorted so the "e.g." location in messages is stable across runs.
    let mut ordered: Vec<(&String, &IndexEntry)> = files.iter().collect();
    ordered.sort_by(|a, b| a.0.cmp(b.0));
    for (file, entry) in ordered {
        let mut classes: Vec<(&String, &Vec<String>)> = entry.class_method_names.iter().collect();
        classes.sort_by(|a, b| a.0.cmp(b.0));
        for (class, methods) in classes {
            if entry.protocol_classes.contains(class) {
                continue;
            }
            for method in methods {
                let contract = entry.contracts.get(&format!("{class}.{method}"));
                let agreed = match (seen.get(method.as_str()), contract) {
                    (None, Some(c)) => Some((c, file.as_str())),
                    (Some(Some((first, at))), Some(c)) if first.agrees_with(c) => {
                        Some((*first, *at))
                    }
                    _ => None,
                };
                seen.insert(method.as_str(), agreed);
            }
        }
    }
    seen.into_iter()
        .filter_map(|(method, agreed)| {
            let (contract, file) = agreed?;
            contract.has_requirements().then(|| {
                (
                    method.to_string(),
                    (
                        contract.clone(),
                        format!(
                            "every '{method}' in the project (e.g. {file}:{})",
                            contract.def_line
                        ),
                    ),
                )
            })
        })
        .collect()
}

// Resolve every function's and self-method's `helper_summary` to what it actually does
// once every file has been indexed. `index_file` computes each one from its own file
// alone (see `Linter::preserve_unresolved_delegates`), so a call to a name defined
// elsewhere -- a plain import, or a base class's method reached via `self` -- is left as
// a raw `Delegate`/`ReturnedFrom` rather than dropped; this finishes that resolution,
// following import and inheritance chains across files, the way `resolve_transitive_requires`
// does for column requirements.
pub(crate) fn resolve_transitive_helper_summaries(
    fs: &dyn crate::ports::FileSystem,
    project_root: &Path,
    files: &mut HashMap<String, IndexEntry>,
) {
    let func_nodes: Vec<FuncNode> = files
        .iter()
        .flat_map(|(file, entry)| {
            entry
                .functions
                .keys()
                .map(move |f| (file.clone(), f.clone()))
        })
        .collect();
    let method_nodes: Vec<MethodNode> = files
        .iter()
        .flat_map(|(file, entry)| {
            entry
                .class_helper_summaries
                .iter()
                .flat_map(move |(class, methods)| {
                    methods
                        .keys()
                        .map(move |m| (file.clone(), class.clone(), m.clone()))
                })
        })
        .collect();

    let mut func_results = Vec::new();
    let mut method_results = Vec::new();
    {
        let files_ref: &HashMap<String, IndexEntry> = files;
        let mut resolver = HelperResolver {
            fs,
            project_root,
            files: files_ref,
            func_memo: HashMap::new(),
            func_visiting: std::collections::HashSet::new(),
            method_memo: HashMap::new(),
            method_visiting: std::collections::HashSet::new(),
        };
        for node in &func_nodes {
            func_results.push((node.clone(), resolver.resolve_function(node)));
        }
        for node in &method_nodes {
            method_results.push((node.clone(), resolver.resolve_method(node)));
        }
    }

    for ((file, func), summary) in func_results {
        if let Some(f) = files
            .get_mut(&file)
            .and_then(|e| e.functions.get_mut(&func))
        {
            f.helper_summary = summary;
        }
    }
    for ((file, class, method), summary) in method_results {
        let Some(methods) = files
            .get_mut(&file)
            .and_then(|e| e.class_helper_summaries.get_mut(&class))
        else {
            continue;
        };
        match summary {
            Some(s) => {
                methods.insert(method, s);
            }
            None => {
                methods.remove(&method);
            }
        }
    }
}

// `class` followed by the file that defines it, when that file isn't `from_file` itself:
// same-file first, else `from_file`'s own `from X import ClassName` / `import module`
// statements -- mirrors `resolve_delegate_target`, checking `class_bases` instead of
// `functions` (a class with zero bases still has an entry -- see its doc comment).
pub(super) fn resolve_class_file(
    fs: &dyn crate::ports::FileSystem,
    from_file: &str,
    class_name: &str,
    project_root: &Path,
    files: &HashMap<String, IndexEntry>,
) -> Option<String> {
    let entry = files.get(from_file)?;
    if entry.class_bases.contains_key(class_name) {
        return Some(from_file.to_string());
    }
    if let Some(module_name) = entry.imports.get(class_name) {
        let target_file = resolve_module_file(fs, module_name, project_root, files)?;
        if files
            .get(&target_file)?
            .class_bases
            .contains_key(class_name)
        {
            return Some(target_file);
        }
    }
    for module_name in entry.module_aliases.values() {
        let Some(target_file) = resolve_module_file(fs, module_name, project_root, files) else {
            continue;
        };
        if files
            .get(&target_file)?
            .class_bases
            .contains_key(class_name)
        {
            return Some(target_file);
        }
    }
    None
}
