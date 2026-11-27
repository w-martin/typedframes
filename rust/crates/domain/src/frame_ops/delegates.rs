use super::*;

/// The column set after `edits`, or `None` when any edit is not understood.
pub(crate) fn apply_edits(base: &[String], edits: &[ColumnEdit]) -> Option<Vec<String>> {
    let mut columns = base.to_vec();
    for edit in edits {
        match edit {
            ColumnEdit::Add(added) => {
                for column in added {
                    if !columns.contains(column) {
                        columns.push(column.clone());
                    }
                }
            }
            ColumnEdit::Remove(removed) => columns.retain(|c| !removed.contains(c)),
            ColumnEdit::Rename(mapping) => {
                for column in &mut columns {
                    if let Some(new_name) = mapping.get(column) {
                        *column = new_name.clone();
                    }
                }
            }
            ColumnEdit::Replace(replacement) => columns = replacement.clone(),
            ColumnEdit::Delegate { .. } | ColumnEdit::ReturnedFrom(_) | ColumnEdit::Unknown => {
                return None;
            }
        }
    }
    Some(columns)
}

/// `class` followed by its ancestors in `bases`, nearest first.
pub(crate) fn class_chain(bases: &HashMap<String, Vec<String>>, class: &str) -> Vec<String> {
    let mut chain: Vec<String> = Vec::new();
    let mut pending = vec![class.to_string()];
    while let Some(next) = pending.pop() {
        if chain.contains(&next) {
            continue;
        }
        if let Some(parents) = bases.get(&next) {
            pending.extend(parents.iter().rev().cloned());
        }
        chain.push(next);
    }
    chain
}

/// Replaces every `Delegate` in `summaries` with the callee's own in-place edits. A
/// callee with no summary (or none in place) contributes nothing; a conditional call to
/// one that edits, and any cycle of delegation, is `Unknown`.
///
/// `preserve_unresolved` is for index-time use only (see `Linter::preserve_unresolved_delegates`):
/// a call to a name this file does not itself define may still be a cross-file helper the
/// project index hasn't resolved yet, so the raw `Delegate`/`ReturnedFrom` is kept rather
/// than dropped, for `resolve_transitive_helper_summaries` to finish later. Never set at
/// check time, where every name the project can resolve is already merged in, so "not
/// found" there really does mean "no effect" -- see that call site.
pub(crate) fn resolve_delegates(
    summaries: &mut HashMap<FnKey, HelperSummary>,
    bases: &HashMap<String, Vec<String>>,
    preserve_unresolved: bool,
) {
    let original = summaries.clone();
    for (key, summary) in summaries.iter_mut() {
        let mut resolver = Resolver {
            original: &original,
            bases,
            stack: vec![key.clone()],
            preserve_unresolved,
        };
        summary.in_place = resolver.expand(&summary.in_place, &key.0);
        if let Some(returned) = &summary.returned {
            summary.returned = resolver.expand_returned(returned, &key.0);
        }
    }
    summaries.retain(|_, s| s.returned.is_some() || !s.in_place.is_empty());
}

struct Resolver<'a> {
    pub(super) original: &'a HashMap<FnKey, HelperSummary>,
    pub(super) bases: &'a HashMap<String, Vec<String>>,
    pub(super) stack: Vec<FnKey>,
    pub(super) preserve_unresolved: bool,
}

impl<'a> Resolver<'a> {
    // The summary `callee` names when called from `owner`'s code: a plain function, or
    // a method of `owner` or one it inherits.
    fn lookup(&self, callee: &FnRef, owner: &Option<String>) -> Option<(FnKey, &'a HelperSummary)> {
        if callee.via_self {
            return class_chain(self.bases, owner.as_ref()?)
                .into_iter()
                .map(|class| (Some(class), callee.name.clone()))
                .find_map(|key| self.original.get(&key).map(|s| (key, s)));
        }
        // `imported_summaries` (merged into `original` before this runs -- see
        // `Linter::helper_summaries`) keys a module-qualified import the same way.
        let class_slot = callee
            .module
            .as_ref()
            .map(|alias| format!("module:{alias}"));
        let key = (class_slot, callee.name.clone());
        self.original.get(&key).map(|s| (key, s))
    }

    fn expand(&mut self, edits: &[ColumnEdit], owner: &Option<String>) -> Vec<ColumnEdit> {
        self.expand_edits(edits, owner)
    }

    fn expand_edits(&mut self, edits: &[ColumnEdit], owner: &Option<String>) -> Vec<ColumnEdit> {
        let mut out = Vec::new();
        for edit in edits {
            if let ControlFlow::Break(_) = self.expand_edit(owner, &mut out, edit) {
                continue;
            }
        }
        out
    }

    fn expand_edit(
        &mut self,
        owner: &Option<String>,
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
        let Some((key, summary)) = self.lookup(callee, owner) else {
            if self.preserve_unresolved {
                out.push(edit.clone());
            }
            return ControlFlow::Break(());
        };
        if self.stack.contains(&key) {
            out.push(ColumnEdit::Unknown);
            return ControlFlow::Break(());
        }
        self.stack.push(key.clone());
        let callee_edits = self.expand(&summary.in_place, &key.0);
        self.stack.pop();
        if callee_edits.is_empty() {
            return ControlFlow::Break(());
        }
        if *conditional {
            out.push(ColumnEdit::Unknown);
        } else {
            out.extend(callee_edits);
        }
        ControlFlow::Continue(())
    }

    // The edits a helper's returned frame ends up with, or `None` when it returns
    // another helper's result and that one is not itself a known pass-through (or is
    // a cycle).
    fn expand_returned(
        &mut self,
        edits: &[ColumnEdit],
        owner: &Option<String>,
    ) -> Option<Vec<ColumnEdit>> {
        let mut out = Vec::new();
        for edit in edits {
            let ColumnEdit::ReturnedFrom(callee) = edit else {
                out.extend(self.expand(std::slice::from_ref(edit), owner));
                continue;
            };
            let Some((key, summary)) = self.lookup(callee, owner) else {
                if let ControlFlow::Break(_) = self.keep_unresolved(&mut out, edit) {
                    continue;
                }
                return None;
            };
            if self.stack.contains(&key) {
                if let ControlFlow::Break(_) = self.keep_unresolved(&mut out, edit) {
                    continue;
                }
                return None;
            }
            let Some(returned) = &summary.returned else {
                if let ControlFlow::Break(_) = self.keep_unresolved(&mut out, edit) {
                    continue;
                }
                return None;
            };
            self.stack.push(key.clone());
            let expanded = self.expand_returned(returned, &key.0);
            self.stack.pop();
            match expanded {
                Some(expanded) => out.extend(expanded),
                None if self.preserve_unresolved => out.push(edit.clone()),
                None => return None,
            }
        }
        Some(out)
    }

    fn keep_unresolved(&mut self, out: &mut Vec<ColumnEdit>, edit: &ColumnEdit) -> ControlFlow<()> {
        if self.preserve_unresolved {
            out.push(edit.clone());
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }
}
