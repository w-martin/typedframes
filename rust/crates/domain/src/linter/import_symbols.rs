use super::*;

impl Linter {
    // Records the column contract of `entry`'s function `name` under `(slot, local)`.
    pub(super) fn import_contract(
        &mut self,
        entry: &IndexEntry,
        name: &str,
        slot: &str,
        local: &str,
        file_path_display: &str,
    ) {
        if let Some(contract) = entry.contracts.get(name) {
            let origin = format!("{name} ({file_path_display}:{})", contract.def_line);
            self.imported_contracts.insert(
                (Some(slot.to_string()), local.to_string()),
                (contract.clone(), origin),
            );
        }
    }

    // Records the helper summary of `entry`'s function `name` under the local name it is
    // called by, in the class slot `slot` (`None` for a plain function).
    pub(super) fn import_helper_summary(
        &mut self,
        entry: &IndexEntry,
        name: &str,
        slot: Option<String>,
        local: &str,
    ) {
        if slot.is_none() {
            if let Some(function) = entry.functions.get(name) {
                if !function.param_usage.is_empty() {
                    self.imported_param_usage
                        .insert(local.to_string(), function.param_usage.clone());
                }
            }
        }
        if let Some(summary) = entry
            .functions
            .get(name)
            .and_then(|f| f.helper_summary.as_ref())
        {
            self.imported_summaries
                .insert((slot, local.to_string()), summary.clone());
        }
    }

    // Pull a single name's schema/function/param-contract info from a resolved
    // cross-file IndexEntry into this Linter's own local state. Shared by
    // `from X import name`, `from X import *`, and plain `import module` handling
    // in load_cross_file_symbols — all three ultimately need to do the same thing
    // for each name they bring into scope.
    pub(super) fn import_name(
        &mut self,
        entry: &IndexEntry,
        name: &str,
        file_path_display: &str,
        all_schemas: &HashMap<String, Vec<String>>,
        all_schema_locations: &HashMap<String, (String, usize)>,
    ) {
        self.import_schema_name(
            entry,
            name,
            file_path_display,
            all_schemas,
            all_schema_locations,
        );
        let Some(func) = entry.functions.get(name) else {
            return;
        };
        if !func.returns_schema.is_empty() {
            self.functions
                .insert(name.to_string(), func.returns_schema.clone());
        }
        if let Some(library) = suggest::Library::from_name(&func.returns_library) {
            self.imported_function_libraries
                .insert(name.to_string(), library);
        }
        // The defining file's own schema first: an inferred name is only unique within one
        // file, and two named schemas in different files can share a name.
        let returned_cols = entry
            .schemas
            .get(func.returns_schema.as_str())
            .or_else(|| all_schemas.get(func.returns_schema.as_str()));
        if let Some(schema_cols) = returned_cols {
            self.schemas
                .insert(func.returns_schema.clone(), schema_cols.clone());
            // Record origin so error messages point back to the source function/file.
            // The inferred schema name encodes the definition line as
            // `__inferred_<var>_at_<line>`, so we can surface the exact
            // location without storing extra metadata.
            if func.returns_schema.starts_with("__inferred_") {
                let line_suffix = inferred_schema_line(&func.returns_schema)
                    .map(|l| format!(":{l}"))
                    .unwrap_or_default();
                self.schema_origins.insert(
                    func.returns_schema.clone(),
                    format!("{name} ({file_path_display}{line_suffix})"),
                );
            } else if let Some(loc) = all_schema_locations.get(func.returns_schema.as_str()) {
                // A named schema reached transitively through this function's
                // return-schema annotation, possibly defined in a THIRD file (neither
                // this one nor the function's own) -- schema_locations wouldn't have
                // picked it up any other way.
                self.schema_locations
                    .insert(func.returns_schema.clone(), loc.clone());
            }
        }
    }

    fn import_schema_name(
        &mut self,
        entry: &IndexEntry,
        name: &str,
        file_path_display: &str,
        all_schemas: &HashMap<String, Vec<String>>,
        all_schema_locations: &HashMap<String, (String, usize)>,
    ) {
        if let Some(cols) = entry.schemas.get(name) {
            self.schemas.insert(name.to_string(), cols.clone());
        }
        if let Some(loc) = entry.schema_locations.get(name) {
            self.schema_locations.insert(name.to_string(), loc.clone());
        }
        // A package's `__init__.py` usually re-exports its schemas (`from .schema import Rows`,
        // often under `TYPE_CHECKING`) rather than defining them, so look the name up project-wide.
        if file_path_display.ends_with("__init__.py") && !self.schemas.contains_key(name) {
            if let Some(cols) = all_schemas.get(name) {
                self.schemas.insert(name.to_string(), cols.clone());
            }
            if let Some(loc) = all_schema_locations.get(name) {
                self.schema_locations.insert(name.to_string(), loc.clone());
            }
        }
        // `name` may be a class (e.g. `from repository import DataRepository`) rather
        // than a function -- entry.functions.get(name) below would be None for that
        // and return early, so this has to run first. Never overwrites an entry this
        // file's OWN class definitions already produced (see class_methods' doc
        // comment): same-file always wins over cross-file, the same priority
        // `current_self_attrs`/`class_methods` resolution already gives same-file
        // information everywhere else.
        if let Some(methods) = entry.class_methods.get(name) {
            self.import_class_methods(entry, name, all_schemas, all_schema_locations, methods);
        }
        if let Some(methods) = entry.class_method_libraries.get(name) {
            for (method, library) in methods {
                if let Some(library) = suggest::Library::from_name(library) {
                    self.imported_class_method_libraries
                        .insert((name.to_string(), method.clone()), library);
                }
            }
        }
        // The class's own methods' fully cross-file-resolved column edits (see
        // `resolve_transitive_helper_summaries`), the `helper_summaries` counterpart to
        // `class_methods` just above -- lets `self.<method>()` inside THIS file's own
        // classes follow a mutating method inherited from `name`, one hop deep. Same
        // same-file-wins guard.
        let prefix = format!("{name}.");
        let classes = if entry.class_bases.contains_key(name) {
            entry.contracts.iter()
        } else {
            Default::default()
        };
        for (key, contract) in classes {
            if let Some(method) = key.strip_prefix(&prefix) {
                let origin = format!("{method} ({file_path_display}:{})", contract.def_line);
                self.imported_contracts
                    .entry((Some(name.to_string()), method.to_string()))
                    .or_insert((contract.clone(), origin));
            }
        }
        if let Some(methods) = entry.class_helper_summaries.get(name) {
            for (method, summary) in methods {
                self.imported_summaries
                    .entry((Some(name.to_string()), method.clone()))
                    .or_insert_with(|| summary.clone());
            }
        }
    }

    fn import_class_methods(
        &mut self,
        entry: &IndexEntry,
        name: &str,
        all_schemas: &HashMap<String, Vec<String>>,
        all_schema_locations: &HashMap<String, (String, usize)>,
        methods: &HashMap<String, String>,
    ) {
        self.class_methods
            .entry(name.to_string())
            .or_insert_with(|| methods.clone());
        // Pull in each named schema's actual column list from the project-wide
        // `all_schemas` registry -- mirroring exactly what the `func.returns_schema`
        // handling below does for a plain imported function, just for every one of
        // this class's methods instead of a single function. Without this,
        // `self.class_methods` would carry the right SCHEMA NAME across files, but
        // `self.schemas[that name]` would stay empty here, since nothing else in
        // this file ever imports the schema class by name directly. Schemas may
        // live in a THIRD file (neither this one nor the class's own), same
        // reasoning as `all_schemas` itself. OPEN_FRAME_MARKER entries need no
        // handling here -- they're resolved into a real, freshly-synthesized open
        // schema entirely at the call site, by `resolve_open_or_named_schema`.
        for schema_name in methods.values() {
            if schema_name == OPEN_FRAME_MARKER || self.schemas.contains_key(schema_name) {
                continue;
            }
            if let Some(cols) = entry
                .schemas
                .get(schema_name)
                .or_else(|| all_schemas.get(schema_name))
            {
                self.schemas.insert(schema_name.clone(), cols.clone());
                if let Some(loc) = all_schema_locations.get(schema_name) {
                    self.schema_locations
                        .insert(schema_name.clone(), loc.clone());
                }
            }
        }
    }
}
