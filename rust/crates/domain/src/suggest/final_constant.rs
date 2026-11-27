use super::*;

/// The one top-level class named `name`, if the file defines exactly one.
fn local_class<'a>(module: &'a ModModule, name: &str) -> Option<&'a ast::StmtClassDef> {
    let mut found = module.body.iter().filter_map(|stmt| match stmt {
        Stmt::ClassDef(c) if c.name.as_str() == name => Some(c),
        _ => None,
    });
    match (found.next(), found.next()) {
        (Some(class), None) => Some(class),
        _ => None,
    }
}

fn is_class_var(expr: &Expr) -> bool {
    match expr {
        Expr::Name(name) => name.id.as_str() == "ClassVar",
        Expr::Attribute(attr) => attr.attr.as_str() == "ClassVar",
        _ => false,
    }
}

pub(super) fn binds_name(stmt: &Stmt, attr: &str) -> bool {
    let is_attr = |target: &Expr| matches!(target, Expr::Name(n) if n.id.as_str() == attr);
    match stmt {
        Stmt::Assign(a) => a.targets.iter().any(is_attr),
        Stmt::AnnAssign(a) => is_attr(&a.target),
        _ => false,
    }
}

/// The name of the class in this file that `attr` resolves to when read through `class`,
/// and its one binding. Bases are searched in order; a base this file does not define
/// might supply `attr` first, so reaching one ends the search.
fn defining_binding<'a>(
    module: &'a ModModule,
    class: &str,
    attr: &str,
    visiting: &mut Vec<String>,
) -> Result<(&'a str, &'a Stmt), String> {
    let Some(class_def) = local_class(module, class) else {
        return Err(format!(
            "class `{class}` is not defined exactly once at the top level of this file"
        ));
    };
    if visiting.iter().any(|seen| seen == class) {
        return Err(format!("`{class}` inherits from itself"));
    }
    visiting.push(class.to_string());
    let mut own = class_def.body.iter().filter(|stmt| binds_name(stmt, attr));
    match (own.next(), own.next()) {
        (Some(binding), None) => return Ok((class_def.name.as_str(), binding)),
        (Some(_), Some(_)) => {
            return Err(format!("`{attr}` is assigned more than once in `{class}`"))
        }
        _ => {}
    }
    for base in class_def.bases() {
        match base {
            Expr::Name(name) if local_class(module, name.id.as_str()).is_some() => {
                if let Ok(found) = defining_binding(module, name.id.as_str(), attr, visiting) {
                    return Ok(found);
                }
            }
            _ => {
                return Err(format!(
                    "`{attr}` is not assigned in `{class}`; it may come from a base class \
                     defined in another file"
                ))
            }
        }
    }
    Err(format!(
        "`{attr}` is not assigned in `{class}` or its bases in this file"
    ))
}

fn descends_from(module: &ModModule, class: &str, ancestor: &str, depth: usize) -> bool {
    let Some(class_def) = local_class(module, class) else {
        return false;
    };
    depth < 32
        && class_def.bases().iter().any(|base| match base {
            Expr::Name(name) => {
                name.id.as_str() == ancestor
                    || descends_from(module, name.id.as_str(), ancestor, depth + 1)
            }
            _ => false,
        })
}

/// A class in this file, below `defining`, that assigns `attr` again.
fn rebinding_subclass<'a>(module: &'a ModModule, defining: &str, attr: &str) -> Option<&'a str> {
    module.body.iter().find_map(|stmt| match stmt {
        Stmt::ClassDef(c)
            if c.name.as_str() != defining
                && c.body.iter().any(|s| binds_name(s, attr))
                && descends_from(module, c.name.as_str(), defining, 0) =>
        {
            Some(c.name.as_str())
        }
        _ => None,
    })
}

/// Whether `name` appears in `source` as an identifier outside the byte ranges `except`.
fn mentions_outside(source: &str, name: &str, except: &[std::ops::Range<usize>]) -> bool {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    source.match_indices(name).any(|(at, _)| {
        let end = at + name.len();
        !except.iter().any(|r| r.start <= at && end <= r.end)
            && !source[..at].chars().next_back().is_some_and(is_ident)
            && !source[end..].chars().next().is_some_and(is_ident)
    })
}

/// The edit that drops `ClassVar` from `from typing import ...` once the rewrite has removed
/// its last use, so the fix does not leave an unused import (`F401`) behind. It reuses the
/// `, Final` insertion in `edits` when `ClassVar` was the only name imported.
fn drop_orphaned_class_var(
    module: &ModModule,
    source: &str,
    edits: &mut Vec<Edit>,
    replaced: std::ops::Range<usize>,
) {
    let Some(import) = module.body.iter().find_map(|stmt| match stmt {
        Stmt::ImportFrom(import)
            if import.level == 0
                && import
                    .module
                    .as_ref()
                    .is_some_and(|m| m.as_str() == "typing")
                && import.names.iter().all(|a| a.name.as_str() != "*") =>
        {
            Some(import)
        }
        _ => None,
    }) else {
        return;
    };
    let Some(position) = import
        .names
        .iter()
        .position(|a| a.name.as_str() == "ClassVar" && a.asname.is_none())
    else {
        return;
    };
    let import_range = import.range().start().to_usize()..import.range().end().to_usize();
    if mentions_outside(source, "ClassVar", &[import_range, replaced]) {
        return;
    }
    let alias = &import.names[position];
    let (start, end) = (
        alias.range().start().to_usize(),
        alias.range().end().to_usize(),
    );
    if import.names.len() == 1 {
        if let Some(insertion) = edits
            .iter_mut()
            .find(|e| e.start == end && e.end == end && e.replacement == ", Final")
        {
            *insertion = Edit {
                start,
                end,
                replacement: "Final".to_string(),
            };
        }
        return;
    }
    let edit = match import.names.get(position + 1) {
        Some(next) => Edit {
            start,
            end: next.range().start().to_usize(),
            replacement: String::new(),
        },
        None => Edit {
            start: import.names[position - 1].range().end().to_usize(),
            end,
            replacement: String::new(),
        },
    };
    edits.push(edit);
}

/// `source` without a `ClassVar` import that nothing uses any more, as left behind when
/// several constants of one file were each rewritten from `ClassVar` to `Final`.
pub(crate) fn without_orphaned_class_var(original: &str, source: &str) -> String {
    // An import that was already unused is not ours to remove.
    let line_of_import = |text: &str| {
        text.lines()
            .find(|l| l.starts_with("from typing import") && l.contains("ClassVar"))
            .map(str::to_string)
    };
    let used_before = line_of_import(original).is_some_and(|import| {
        let rest = original.replacen(&import, "", 1);
        mentions_outside(&rest, "ClassVar", &[])
    });
    if !used_before {
        return source.to_string();
    }
    let Ok(module) = ruff_python_parser::parse_module(source) else {
        return source.to_string();
    };
    let module = module.into_syntax();
    let mut edits = Vec::new();
    drop_orphaned_class_var(&module, source, &mut edits, 0..0);
    // Only a plain removal: the `, Final` swap belongs to the single-constant path.
    edits.retain(|e| e.replacement.is_empty());
    crate::edits::apply_edits(source, &edits).unwrap_or_else(|| source.to_string())
}

/// `NAME: Final = ...` for a class constant read through `self`, so no subclass in another
/// file can rebind what the checker resolved. Withheld unless the constant is one plain
/// assignment directly in the body of a top-level class of this file, the reading class or
/// a base of it, and no class below that one assigns it again.
pub(crate) fn suggest_final_constant(
    module: &ModModule,
    source: &str,
    display_path: &str,
    class: &str,
    attr: &str,
) -> Result<Suggestion, String> {
    let (defining, binding) = defining_binding(module, class, attr, &mut Vec::new())?;
    if let Some(rebinder) = rebinding_subclass(module, defining, attr) {
        return Err(format!(
            "`{rebinder}` rebinds `{attr}`, which `Final` would forbid"
        ));
    }
    let class = defining;

    let (edits, edited, line, col) = final_constant_target(module, source, attr, binding)?;
    Ok(Suggestion {
        phrase: "assumed-constant".to_string(),
        kind: "final",
        line,
        col,
        var: attr.to_string(),
        tier: "safe",
        columns: Vec::new(),
        reason: format!(
            "`{attr}` of `{class}` is read through `self` as a constant; declaring it `Final` \
             rules out a subclass in another file rebinding it (a type checker will flag one)"
        ),
        notes: Vec::new(),
        diff: render_unified_diff(display_path, source, &edited),
        edits,
        cell: None,
    })
}

fn final_constant_target(
    module: &ModModule,
    source: &str,
    attr: &str,
    binding: &Stmt,
) -> Result<(Vec<Edit>, String, usize, usize), String> {
    let (target, mut edits) = match binding {
        Stmt::Assign(assign) => {
            let [target] = assign.targets.as_slice() else {
                return Err(format!("`{attr}` is assigned in a chained assignment"));
            };
            (target, Vec::new())
        }
        Stmt::AnnAssign(ann) => {
            if ann.value.is_none() {
                return Err(format!("`{attr}` is annotated but not assigned a value"));
            }
            (&*ann.target, Vec::new())
        }
        _ => unreachable!("filtered to assignments above"),
    };
    let final_ref = match final_name(module, source)? {
        FinalName::Imported(name) => name,
        FinalName::Add(edit) => {
            edits.push(edit);
            "Final".to_string()
        }
    };
    match binding {
        Stmt::AnnAssign(ann) => {
            let range = ann.annotation.range();
            // `Final` already means a class-level constant, and `Final[ClassVar[T]]` is invalid.
            let replacement = match &*ann.annotation {
                Expr::Subscript(sub) if is_class_var(&sub.value) => {
                    format!("{final_ref}[{}]", &source[sub.slice.range()])
                }
                bare if is_class_var(bare) => final_ref.clone(),
                _ => format!("{final_ref}[{}]", &source[range]),
            };
            edits.push(Edit {
                start: range.start().to_usize(),
                end: range.end().to_usize(),
                replacement,
            });
            drop_orphaned_class_var(
                module,
                source,
                &mut edits,
                range.start().to_usize()..range.end().to_usize(),
            );
        }
        _ => {
            let end = target.range().end().to_usize();
            edits.push(Edit {
                start: end,
                end,
                replacement: format!(": {final_ref}"),
            });
        }
    }
    let edited = crate::edits::apply_edits(source, &edits)
        .ok_or_else(|| "its edits overlap or don't fit the file".to_string())?;
    let line = source[..target.range().start().to_usize()]
        .matches('\n')
        .count()
        + 1;
    let col = target.range().start().to_usize()
        - source[..target.range().start().to_usize()]
            .rfind('\n')
            .map_or(0, |i| i + 1)
        + 1;
    Ok((edits, edited, line, col))
}

// The lines between the trimmed ends, in order. Edits in two places leave the code between
// them in both versions; shown as context rather than removed and added again. Past a size
// where the table gets large, all removals then all additions.
pub(super) fn interleave<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<(char, &'a str)> {
    let (n, m) = (old.len(), new.len());
    if n == 0 || m == 0 || n.saturating_mul(m) > MAX_CELLS {
        let removed = old.iter().map(|l| ('-', *l));
        return removed.chain(new.iter().map(|l| ('+', *l))).collect();
    }
    // shared[i][j]: the longest run of lines old[i..] and new[j..] have in common.
    let mut shared = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            shared[i][j] = if old[i] == new[j] {
                shared[i + 1][j + 1] + 1
            } else {
                shared[i + 1][j].max(shared[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::with_capacity(n + m);
    while i < n || j < m {
        if i < n && j < m && old[i] == new[j] {
            out.push((' ', old[i]));
            i += 1;
            j += 1;
        } else if j == m || (i < n && shared[i + 1][j] >= shared[i][j + 1]) {
            out.push(('-', old[i]));
            i += 1;
        } else {
            out.push(('+', new[j]));
            j += 1;
        }
    }
    out
}
