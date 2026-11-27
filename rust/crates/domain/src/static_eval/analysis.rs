use super::*;

pub(crate) struct Analysis<'a> {
    /// Names of other modules this file may change: see `ForeignWrite`.
    pub(crate) foreign_writes: Vec<ForeignWrite>,
    pub(super) groups: FastMap<GroupKey<'a>, Group<'a>>,
    pub(super) occurrence_group: FastMap<TextSize, Option<GroupKey<'a>>>,
    pub(super) attribute_group: FastMap<TextRange, Option<GroupKey<'a>>>,
    // `cfg.NAME` where `cfg` is an imported module: the module's group and the member,
    // plus how this file uses that member (every use of it, across the whole file).
    pub(super) module_attribute: FastMap<TextRange, (GroupKey<'a>, &'a str)>,
    pub(super) module_member_use: FastMap<(GroupKey<'a>, &'a str), (bool, bool)>,
    // `Col.HR` where `Col` is a class imported with `from module import Col`: the import and the attribute.
    pub(super) imported_class_member: FastMap<TextRange, (GroupKey<'a>, &'a str)>,
    // The attributes of this file's own module-level classes that importers may read.
    pub(super) class_exports: Vec<(String, String, GroupKey<'a>)>,
    // This file's module-level string enums and every name their body binds.
    pub(super) string_enums: FastMap<&'a str, Vec<String>>,
    // Class constants read through `self`/`cls`, where a subclass in another file could
    // have rebound the name.
    pub(super) self_reads: Vec<SelfConstantRead>,
    pub(super) imports: &'a ImportedModules,
    pub(super) shadowed: Vec<String>,
    pub(super) complete: bool,
    pub(super) dynamic_namespace: bool,
    // Keyed by a group's address, which is stable because `groups` is not changed once built.
    pub(super) memo: RefCell<FastMap<usize, Memo>>,
    pub(super) depth: Cell<u32>,
    // Functions being evaluated right now (a recursive one is refused) and how many calls
    // have been evaluated in total.
    pub(super) call_stack: RefCell<Vec<usize>>,
    pub(super) calls: Cell<u32>,
}

pub(super) const MAX_CALL_DEPTH: usize = 16;

pub(super) const MAX_CALLS: u32 = 10_000;

impl<'a> Analysis<'a> {
    /// Forgets what evaluating has cached and used up, so the next user of a shared analysis
    /// gets the budgets and results a freshly built one would have.
    pub(crate) fn reset(&self) {
        self.memo.borrow_mut().clear();
        self.depth.set(0);
        self.call_stack.borrow_mut().clear();
        self.calls.set(0);
    }

    pub(crate) fn build(module: &'a ModModule, imports: &'a ImportedModules) -> Self {
        let (collector, complete) = collect_names(module);

        let scopes = &collector.scopes;
        let mut groups = bound_groups(&collector, imports);

        let classes = ClassIndex::new(&groups);
        let dynamic_classes = dynamic_classes(&collector, &classes, &groups);
        count_attribute_stores(&collector, &classes, &dynamic_classes, &mut groups);

        let occurrence_group = occurrence_groups(&collector, &mut groups);
        let bound_anywhere = |name: &str| scopes.iter().any(|s| s.bindings.contains_key(name));

        let AttributeIndex {
            attribute_group,
            self_reads,
            module_attribute,
            module_member_use,
            imported_class_member,
        } = attribute_index(&collector, &mut groups, &classes, &dynamic_classes);

        let foreign_writes = foreign_writes(&groups, scopes, &module_member_use, &collector);

        let (class_exports, string_enums) =
            class_exports_and_string_enums(&groups, scopes, &dynamic_classes);

        Analysis {
            foreign_writes,
            groups,
            occurrence_group,
            attribute_group,
            module_attribute,
            module_member_use,
            imported_class_member,
            class_exports,
            string_enums,
            self_reads,
            imports,
            shadowed: EVALUATED_BUILTINS
                .iter()
                .filter(|b| bound_anywhere(b))
                .map(|b| b.to_string())
                .collect(),
            complete,
            dynamic_namespace: collector.dynamic_namespace,
            memo: RefCell::new(FastMap::default()),
            depth: Cell::new(0),
            call_stack: RefCell::new(Vec::new()),
            calls: Cell::new(0),
        }
    }

    pub(super) fn group_value(&self, key: &GroupKey<'a>) -> Option<Value> {
        let id = self.group_id(key)?;
        match self.memo.borrow().get(&id) {
            Some(Memo::InProgress) => return None,
            Some(Memo::Done(v)) => return v.clone(),
            None => {}
        }
        let depth = self.depth.get() + 1;
        if depth > MAX_DEPTH {
            return None;
        }
        self.depth.set(depth);
        self.memo.borrow_mut().insert(id, Memo::InProgress);
        let value = self.compute(key);
        self.depth.set(depth - 1);
        self.memo.borrow_mut().insert(id, Memo::Done(value.clone()));
        value
    }

    // A stable identity for the group `key` names, if there is one.
    fn group_id(&self, key: &GroupKey<'a>) -> Option<usize> {
        self.groups
            .get(key)
            .map(|group| std::ptr::from_ref(group) as usize)
    }

    fn compute(&self, key: &GroupKey<'a>) -> Option<Value> {
        let group = self.groups.get(key)?;
        // One plain assignment, and a module-level name only if nothing can rewrite the
        // module namespace behind the bindings' backs.
        if group.bindings != 1 || (key.0 == 0 && self.dynamic_namespace) {
            return None;
        }
        let value = match (&group.assign, &group.import) {
            (Some(rhs), _) => Evaluator::new(self).eval(rhs)?,
            (None, Some(ImportRef::From(level, module, name))) => self
                .imports
                .get(&(*level, module.clone()))?
                .constants
                .get(name)?
                .clone(),
            _ => return None,
        };
        // A value nothing can change may be used any way at all. A container may not be
        // aliased, passed on or stored (any of which could change it unseen), and may
        // hand out its elements only if they are immutable.
        if group.escapes && !value.is_immutable() {
            return None;
        }
        if group.interior && !value.has_immutable_elements() {
            return None;
        }
        Some(value)
    }
}
