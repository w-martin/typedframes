use super::*;

#[derive(Clone)]
pub(super) enum ImportRef {
    From(u32, String, String),
    // `import a.b as m`, or `from a import b` where `b` is a submodule: (leading dots, module).
    Module(u32, String),
}

/// How an imported name was brought in: `import x [as y]`, or `from x import y`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ImportKind {
    Module,
    From,
}

#[derive(Default)]
pub(super) struct Group<'a> {
    pub(super) bindings: usize,
    pub(super) assign: Option<&'a Expr>,
    pub(super) function: Option<&'a ast::StmtFunctionDef>,
    pub(super) class: Option<(&'a ast::StmtClassDef, usize)>,
    pub(super) import: Option<ImportRef>,
    pub(super) interior: bool,
    pub(super) escapes: bool,
}

pub(super) enum Memo {
    InProgress,
    Done(Option<Value>),
}

/// A module-level name of another module that this file may change: `COLS.append(..)` after
/// `from m import COLS` (`rebinds` false: only a mutable value is affected), or `m.NAME = ..`
/// (`rebinds`: any value is). The module is `(leading dots, dotted name)` as written here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ForeignWrite {
    pub(crate) level: u32,
    pub(crate) module: String,
    pub(crate) name: String,
    pub(crate) rebinds: bool,
}

pub(super) fn foreign_writes<'a>(
    groups: &FastMap<GroupKey<'a>, Group<'a>>,
    scopes: &[Scope<'a>],
    member_use: &FastMap<(GroupKey<'a>, &'a str), (bool, bool)>,
    collector: &Collector<'a>,
) -> Vec<ForeignWrite> {
    let mut out = Vec::new();
    // The module(s) `base.attr` may name: `import m` -> m.attr; `from p import x` -> p.x.attr
    // (x a submodule) or p's `x.attr` (x a class).
    let mut member = |base: &GroupKey<'a>, attr: &str, rebinds: bool| match groups
        .get(base)
        .and_then(|g| g.import.as_ref())
    {
        Some(ImportRef::Module(level, module)) => out.push(ForeignWrite {
            level: *level,
            module: module.clone(),
            name: attr.to_string(),
            rebinds,
        }),
        Some(ImportRef::From(level, module, imported)) => {
            let submodule = if module.is_empty() {
                imported.clone()
            } else {
                format!("{module}.{imported}")
            };
            out.push(ForeignWrite {
                level: *level,
                module: submodule,
                name: attr.to_string(),
                rebinds,
            });
            out.push(ForeignWrite {
                level: *level,
                module: module.clone(),
                name: format!("{imported}.{attr}"),
                rebinds,
            });
        }
        None => {}
    };
    for ((base, attr), (_, escapes)) in member_use {
        if *escapes {
            member(base, attr, false);
        }
    }
    for (scope, base, attr) in &collector.pending_attribute_stores {
        if let Some(key) = lookup(scopes, *scope, base) {
            member(&key, attr, true);
        }
    }
    for group in groups.values() {
        if let (true, Some(ImportRef::From(level, module, imported))) =
            (group.escapes, group.import.as_ref())
        {
            out.push(ForeignWrite {
                level: *level,
                module: module.clone(),
                name: imported.clone(),
                rebinds: false,
            });
        }
    }
    out.sort_by(|a, b| (a.level, &a.module, &a.name).cmp(&(b.level, &b.module, &b.name)));
    out.dedup();
    out
}

/// A class constant read through `self`/`cls` and resolved to its value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SelfConstantRead {
    pub(crate) range: TextRange,
    pub(crate) attr: String,
    pub(crate) class: String,
    /// Declared `NAME: Final = ...`, so no subclass can rebind it.
    pub(crate) is_final: bool,
}
