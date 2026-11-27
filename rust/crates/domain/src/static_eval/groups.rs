use super::*;

// Every name's bindings, by scope, with what a lone binding binds.
pub(super) fn bound_groups<'a>(
    collector: &Collector<'a>,
    imports: &'a ImportedModules,
) -> FastMap<GroupKey<'a>, Group<'a>> {
    let scopes = &collector.scopes;
    let mut groups: FastMap<GroupKey<'a>, Group<'a>> = FastMap::default();
    for (id, scope) in scopes.iter().enumerate() {
        for (name, bindings) in &scope.bindings {
            let group = groups.entry((id, name)).or_default();
            group.bindings += bindings.len();
            if let [only] = bindings.as_slice() {
                match only {
                    Binding::Assign(rhs) => group.assign = Some(rhs),
                    Binding::Function(f) => group.function = Some(f),
                    Binding::Class(c, body) => group.class = Some((c, *body)),
                    Binding::ImportFrom(level, module, name) => {
                        group.import = Some(import_from_ref(imports, *level, module, name))
                    }
                    Binding::ImportModule(module) => {
                        group.import = Some(ImportRef::Module(0, module.clone()))
                    }
                    Binding::Other => {}
                }
            }
        }
    }
    for (scope, name) in &collector.pending_nonlocal {
        if let Some(key) = lookup(scopes, *scope, name) {
            groups.entry(key).or_default().bindings += 1;
        }
    }
    groups
}

// Classes whose attributes `setattr`/`delattr`/`vars` may change, or that intercept attribute
// assignment, by the scope of their body.
pub(super) fn dynamic_classes<'a>(
    collector: &Collector<'a>,
    classes: &ClassIndex<'a>,
    groups: &FastMap<GroupKey<'a>, Group<'a>>,
) -> FastSet<usize> {
    let scopes = &collector.scopes;
    // A class whose attributes `setattr`/`delattr`/`vars` may change, and stores to a
    // class attribute, which are one more binding of it.
    let mut dynamic_classes: FastSet<usize> = FastSet::default();
    for (scope, name) in &collector.dynamic_class_targets {
        let class = enclosing_class(classes, scopes, *scope, name)
            .or_else(|| lookup(scopes, *scope, name))
            .and_then(|key| groups.get(&key))
            .and_then(|g| g.class);
        if let Some((_, body)) = class {
            dynamic_classes.insert(body);
        }
    }
    // A class that intercepts attribute assignment can give any attribute any value.
    for group in groups.values() {
        if let Some((_, body)) = group.class {
            if scopes[body].bindings.contains_key("__setattr__") {
                dynamic_classes.insert(body);
            }
        }
    }
    dynamic_classes
}

// Stores to a class attribute: one more binding of it.
pub(super) fn count_attribute_stores<'a>(
    collector: &Collector<'a>,
    classes: &ClassIndex<'a>,
    dynamic_classes: &FastSet<usize>,
    groups: &mut FastMap<GroupKey<'a>, Group<'a>>,
) {
    let scopes = &collector.scopes;
    for (scope, base, attr) in &collector.pending_attribute_stores {
        if let Some(key) =
            attribute_group_of(groups, classes, scopes, dynamic_classes, *scope, base, attr)
        {
            if let Some(group) = groups.get_mut(&key) {
                group.bindings += 1;
            }
        }
    }
}

// The group each name occurrence belongs to, recording on the group how the name is used.
pub(super) fn occurrence_groups<'a>(
    collector: &Collector<'a>,
    groups: &mut FastMap<GroupKey<'a>, Group<'a>>,
) -> FastMap<TextSize, Option<GroupKey<'a>>> {
    let scopes = &collector.scopes;
    let bound_anywhere = |name: &str| scopes.iter().any(|s| s.bindings.contains_key(name));
    let mut occurrence_group = FastMap::default();
    for occurrence in &collector.occurrences {
        let key = lookup(scopes, occurrence.scope, occurrence.name)
            .filter(|key| groups.contains_key(key));
        let mut use_ = occurrence.use_;
        if occurrence.builtin.is_some_and(bound_anywhere) {
            use_ = Use::Escapes;
        }
        if let Some(key) = &key {
            if let Some(group) = groups.get_mut(key) {
                match use_ {
                    Use::Safe => {}
                    Use::Interior => group.interior = true,
                    Use::Escapes => group.escapes = true,
                }
            }
        }
        occurrence_group.insert(occurrence.offset, key);
    }
    occurrence_group
}

// An attribute of an imported module (`np.nan`) or of a class imported by name: how it is used.
fn record_module_member<'a>(
    groups: &FastMap<GroupKey<'a>, Group<'a>>,
    scopes: &[Scope<'a>],
    occurrence: &AttributeOccurrence<'a>,
    module_member_use: &mut FastMap<(GroupKey<'a>, &'a str), (bool, bool)>,
    module_attribute: &mut FastMap<TextRange, (GroupKey<'a>, &'a str)>,
    imported_class_member: &mut FastMap<TextRange, (GroupKey<'a>, &'a str)>,
) {
    let module_group = lookup(scopes, occurrence.scope, occurrence.base)
        .filter(|k| groups.get(k).is_some_and(|g| g.bindings == 1));
    if let Some(base) = module_group.filter(|k| {
        matches!(
            groups.get(k).and_then(|g| g.import.as_ref()),
            Some(ImportRef::Module(..))
        )
    }) {
        let member = (base, occurrence.attr);
        let flags = module_member_use.entry(member).or_default();
        note_use(occurrence, flags);
        module_attribute.insert(occurrence.range, member);
    } else if let Some(base) = module_group.filter(|k| {
        matches!(
            groups.get(k).and_then(|g| g.import.as_ref()),
            Some(ImportRef::From(..))
        )
    }) {
        let member = (base, occurrence.attr);
        let flags = module_member_use.entry(member).or_default();
        note_use(occurrence, flags);
        imported_class_member.insert(occurrence.range, member);
    }
}

// The attributes of the module's classes that are fixed, by class and attribute name, and the
// members of its string enums.
pub(super) fn class_exports_and_string_enums<'a>(
    groups: &FastMap<GroupKey<'a>, Group<'a>>,
    scopes: &[Scope<'a>],
    dynamic_classes: &FastSet<usize>,
) -> (ClassExports<'a>, FastMap<&'a str, Vec<String>>) {
    let mut class_exports = Vec::new();
    let mut string_enums = FastMap::default();
    for (key, group) in groups {
        let Some((def, body)) = group.class else {
            continue;
        };
        if key.0 != 0 || group.bindings != 1 || dynamic_classes.contains(&body) {
            continue;
        }
        if let Some(bodies) = string_enum_bodies(groups, scopes, def, body) {
            let members = string_enum_members(scopes, bodies);
            string_enums.insert(key.1, members);
        }
        for attr in scopes[body].bindings.keys() {
            if let Some(member) = class_attribute_of(groups, scopes, dynamic_classes, *key, attr) {
                class_exports.push((key.1.to_string(), attr.to_string(), member));
            }
        }
    }
    // `N.a` with `N = Names()`, `Names` a frozen dataclass: the field's default.
    for key in groups.keys() {
        if key.0 != 0 {
            continue;
        }
        let Some(class) = frozen_dataclass_instance_class(groups, scopes, *key) else {
            continue;
        };
        let Some((_, body)) = groups.get(&class).and_then(|g| g.class) else {
            continue;
        };
        for attr in scopes[body].bindings.keys() {
            if let Some(member) = class_attribute_of(groups, scopes, dynamic_classes, class, attr) {
                class_exports.push((key.1.to_string(), attr.to_string(), member));
            }
        }
    }
    (class_exports, string_enums)
}

// The module's names, as the scopes binding them, and whether every name it loads was seen.
pub(super) fn collect_names(module: &ModModule) -> (Collector<'_>, bool) {
    let mut collector = Collector::new();
    collector.body(&module.body);

    let mut loads = AllNameLoads {
        offsets: HashSet::new(),
    };
    for stmt in &module.body {
        loads.visit_stmt(stmt);
    }
    let recorded: HashSet<TextSize> = collector.occurrences.iter().map(|o| o.offset).collect();
    let complete = loads.offsets.iter().all(|o| recorded.contains(o));
    (collector, complete)
}

// Where each attribute occurrence leads: its group, the module or class member it reads, and the
// `self` constants read through it.
pub(super) struct AttributeIndex<'a> {
    pub(super) attribute_group: FastMap<TextRange, Option<GroupKey<'a>>>,
    pub(super) self_reads: Vec<SelfConstantRead>,
    pub(super) module_attribute: FastMap<TextRange, (GroupKey<'a>, &'a str)>,
    pub(super) module_member_use: FastMap<(GroupKey<'a>, &'a str), (bool, bool)>,
    pub(super) imported_class_member: FastMap<TextRange, (GroupKey<'a>, &'a str)>,
}

// Indexes every attribute occurrence, recording on its group how the attribute is used.
pub(super) fn attribute_index<'a>(
    collector: &Collector<'a>,
    groups: &mut FastMap<GroupKey<'a>, Group<'a>>,
    classes: &ClassIndex<'a>,
    dynamic_classes: &FastSet<usize>,
) -> AttributeIndex<'a> {
    let scopes = &collector.scopes;
    let mut attribute_group = FastMap::default();
    let mut self_reads = Vec::new();
    let mut module_attribute = FastMap::default();
    let mut module_member_use: FastMap<(GroupKey<'a>, &'a str), (bool, bool)> = FastMap::default();
    let mut imported_class_member = FastMap::default();
    for occurrence in &collector.attribute_occurrences {
        let key = attribute_group_of(
            groups,
            classes,
            scopes,
            dynamic_classes,
            occurrence.scope,
            occurrence.base,
            occurrence.attr,
        );
        if let Some(group) = key.as_ref().and_then(|k| groups.get_mut(k)) {
            match occurrence.use_ {
                Use::Safe => {}
                Use::Interior => group.interior = true,
                Use::Escapes => group.escapes = true,
            }
        }
        record_self_constant_read(collector, scopes, classes, &mut self_reads, occurrence, key);
        if key.is_none() {
            record_module_member(
                groups,
                scopes,
                occurrence,
                &mut module_member_use,
                &mut module_attribute,
                &mut imported_class_member,
            );
        }
        attribute_group.insert(occurrence.range, key);
    }
    AttributeIndex {
        attribute_group,
        self_reads,
        module_attribute,
        module_member_use,
        imported_class_member,
    }
}

fn string_enum_members(scopes: &[Scope<'_>], bodies: Vec<usize>) -> Vec<String> {
    let mut members: Vec<String> = bodies
        .iter()
        .flat_map(|b| scopes[*b].bindings.keys())
        .map(|m| m.to_string())
        .collect();
    members.sort();
    members.dedup();
    members
}

fn record_self_constant_read(
    collector: &Collector<'_>,
    scopes: &Vec<Scope<'_>>,
    classes: &ClassIndex<'_>,
    self_reads: &mut Vec<SelfConstantRead>,
    occurrence: &AttributeOccurrence<'_>,
    key: Option<(usize, &str)>,
) {
    if let Some(key) = key.as_ref() {
        if let Some(class) = enclosing_class(classes, scopes, occurrence.scope, occurrence.base) {
            self_reads.push(SelfConstantRead {
                range: occurrence.range,
                attr: occurrence.attr.to_string(),
                class: class.1.to_string(),
                is_final: collector.final_bindings.contains(key),
            });
        }
    }
}

fn note_use(occurrence: &AttributeOccurrence<'_>, flags: &mut (bool, bool)) {
    match occurrence.use_ {
        Use::Safe => {}
        Use::Interior => flags.0 = true,
        Use::Escapes => flags.1 = true,
    }
}
