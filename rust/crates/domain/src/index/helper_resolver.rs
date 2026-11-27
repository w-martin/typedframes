use super::*;

// A method identified by the file and class that define it, and its name.
pub(super) type MethodNode = (String, String, String);

// Resolves both plain-function (`FuncNode`) and self-method (`MethodNode`) helper
// summaries project-wide, sharing one pair of memo/cycle-guard sets between the two
// since a method's body can delegate to a plain function and vice versa. A cycle -- of
// either kind, in either direction -- traps every node currently on its own stack, not
// just the one that closed the loop; contributing no further information there is
// `Unknown` for an in-place edit (matching `frame_ops::resolve_delegates`'s own cycle
// handling: silently treating a self-referential edit chain as "no effect" would be a
// false claim, not just an incomplete one) and total failure (`None`) for a returned
// one, same reasoning as an unplaceable callee below.
pub(super) struct HelperResolver<'a> {
    pub(super) fs: &'a dyn crate::ports::FileSystem,
    pub(super) project_root: &'a Path,
    pub(super) files: &'a HashMap<String, IndexEntry>,
    pub(super) func_memo: HashMap<FuncNode, Option<HelperSummary>>,
    pub(super) func_visiting: std::collections::HashSet<FuncNode>,
    pub(super) method_memo: HashMap<MethodNode, Option<HelperSummary>>,
    pub(super) method_visiting: std::collections::HashSet<MethodNode>,
}

impl HelperResolver<'_> {
    pub(super) fn resolve_function(&mut self, node: &FuncNode) -> Option<HelperSummary> {
        if let Some(cached) = self.func_memo.get(node) {
            return cached.clone();
        }
        let Some(raw) = self
            .files
            .get(&node.0)
            .and_then(|e| e.functions.get(&node.1))
            .and_then(|f| f.helper_summary.clone())
        else {
            self.func_memo.insert(node.clone(), None);
            return None;
        };

        self.func_visiting.insert(node.clone());
        let in_place = self.resolve_in_place(&raw.in_place, &node.0, None);
        let returned = raw
            .returned
            .as_ref()
            .and_then(|edits| self.resolve_returned(edits, &node.0, None));
        self.func_visiting.remove(node);

        let result = (returned.is_some() || !in_place.is_empty())
            .then_some(HelperSummary { returned, in_place });
        self.func_memo.insert(node.clone(), result.clone());
        result
    }

    // A `self.<method>()` call, resolved to whichever class in the chain actually defines
    // it -- own class first, then its bases in declaration order, crossing files via
    // `resolve_class_file` for one not defined in the same file -- the same priority
    // `Linter::resolve_self_method` uses at check time. Memoized by the QUERYING
    // (file, class, method), not the eventual defining site, so two subclasses sharing an
    // inherited method each resolve it once, independently -- more work than sharing a
    // single cache entry, never wrong.
    pub(super) fn resolve_method(&mut self, node: &MethodNode) -> Option<HelperSummary> {
        if let Some(cached) = self.method_memo.get(node) {
            return cached.clone();
        }
        if self.method_visiting.contains(node) {
            return None;
        }
        self.method_visiting.insert(node.clone());
        let (file, class, method) = node;
        let result = self.resolve_method_uncached(file, class, method);
        self.method_visiting.remove(node);
        self.method_memo.insert(node.clone(), result.clone());
        result
    }

    fn resolve_method_uncached(
        &mut self,
        file: &str,
        class: &str,
        method: &str,
    ) -> Option<HelperSummary> {
        if let Some(raw) = self
            .files
            .get(file)
            .and_then(|e| e.class_helper_summaries.get(class))
            .and_then(|m| m.get(method))
            .cloned()
        {
            let in_place = self.resolve_in_place(&raw.in_place, file, Some(class));
            let returned = raw
                .returned
                .as_ref()
                .and_then(|edits| self.resolve_returned(edits, file, Some(class)));
            return (returned.is_some() || !in_place.is_empty())
                .then_some(HelperSummary { returned, in_place });
        }
        // Not defined on `class` itself: walk its bases, nearest (declaration order)
        // first, each resolved to its own defining file before recursing.
        let bases = self
            .files
            .get(file)
            .and_then(|e| e.class_bases.get(class))
            .cloned()
            .unwrap_or_default();
        for base in bases {
            let same_file = self
                .files
                .get(file)
                .is_some_and(|e| e.class_bases.contains_key(&base));
            let base_file = if same_file {
                Some(file.to_string())
            } else {
                resolve_class_file(self.fs, file, &base, self.project_root, self.files)
            };
            let Some(base_file) = base_file else {
                continue;
            };
            if let Some(summary) = self.resolve_method(&(base_file, base, method.to_string())) {
                return Some(summary);
            }
        }
        None
    }

    fn resolve_call_target(
        &self,
        file: &str,
        callee: &crate::frame_ops::FnRef,
    ) -> Option<FuncNode> {
        match &callee.module {
            Some(module) => resolve_module_call_target(
                self.fs,
                file,
                module,
                &callee.name,
                self.project_root,
                self.files,
            ),
            None => {
                resolve_delegate_target(self.fs, file, &callee.name, self.project_root, self.files)
            }
        }
    }

    // A `Delegate` to a callee nothing in the project defines (`print`, a stdlib call, an
    // external package never traced) is a confirmed no-op, same as at check time; one
    // this file's own import map (or, for `self`/`cls`, class hierarchy) places
    // elsewhere is resolved by recursing into that definition's own (also fully
    // resolved) summary.
    fn resolve_in_place(
        &mut self,
        edits: &[ColumnEdit],
        file: &str,
        self_class: Option<&str>,
    ) -> Vec<ColumnEdit> {
        let mut out = Vec::new();
        for edit in edits {
            if let ControlFlow::Break(_) =
                self.resolve_in_place_edit(file, self_class, &mut out, edit)
            {
                continue;
            }
        }
        out
    }

    fn resolve_in_place_edit(
        &mut self,
        file: &str,
        self_class: Option<&str>,
        out: &mut Vec<ColumnEdit>,
        edit: &ColumnEdit,
    ) -> ControlFlow<()> {
        let ColumnEdit::Delegate {
            callee,
            conditional,
        } = edit
        else {
            out.push(edit.clone());
            return ControlFlow::Break(());
        };
        if callee.via_self {
            if let Some(value) =
                self.resolve_self_delegate(file, self_class, out, callee, conditional)
            {
                return value;
            }
            return ControlFlow::Break(());
        }
        let Some(target) = self.resolve_call_target(file, callee) else {
            return ControlFlow::Break(());
        };
        if self.func_visiting.contains(&target) {
            out.push(ColumnEdit::Unknown);
            return ControlFlow::Break(());
        }
        match self.resolve_function(&target) {
            Some(summary) if !summary.in_place.is_empty() => {
                if *conditional {
                    out.push(ColumnEdit::Unknown);
                } else {
                    out.extend(summary.in_place);
                }
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }

    fn resolve_self_delegate(
        &mut self,
        file: &str,
        self_class: Option<&str>,
        out: &mut Vec<ColumnEdit>,
        callee: &crate::frame_ops::FnRef,
        conditional: &bool,
    ) -> Option<ControlFlow<()>> {
        let Some(class) = self_class else {
            return Some(ControlFlow::Break(()));
        };
        let key = (file.to_string(), class.to_string(), callee.name.clone());
        if self.method_visiting.contains(&key) {
            out.push(ColumnEdit::Unknown);
            return Some(ControlFlow::Break(()));
        }
        match self.resolve_method(&key) {
            Some(summary) if !summary.in_place.is_empty() => {
                if *conditional {
                    out.push(ColumnEdit::Unknown);
                } else {
                    out.extend(summary.in_place);
                }
            }
            _ => {}
        }
        None
    }

    // A `ReturnedFrom` a callee nowhere in the project defines, or whose own returned
    // edits aren't known either, means the WHOLE return is opaque -- `None` -- rather
    // than a confirmed no-op: unlike a call whose result is discarded, a function's
    // return value is exactly what a caller assigning it relies on, so an unplaceable
    // one must not be reported as a safe pass-through.
    fn resolve_returned(
        &mut self,
        edits: &[ColumnEdit],
        file: &str,
        self_class: Option<&str>,
    ) -> Option<Vec<ColumnEdit>> {
        let mut out = Vec::new();
        for edit in edits {
            let ColumnEdit::ReturnedFrom(callee) = edit else {
                out.extend(self.resolve_in_place(std::slice::from_ref(edit), file, self_class));
                continue;
            };
            if callee.via_self {
                let class = self_class?;
                let key = (file.to_string(), class.to_string(), callee.name.clone());
                if self.method_visiting.contains(&key) {
                    return None;
                }
                let summary = self.resolve_method(&key)?;
                out.extend(summary.returned?);
                continue;
            }
            let target = self.resolve_call_target(file, callee)?;
            if self.func_visiting.contains(&target) {
                return None;
            }
            let summary = self.resolve_function(&target)?;
            out.extend(summary.returned?);
        }
        Some(out)
    }
}
