use super::*;

impl Linter {
    // Registers the schema a `BaseSchema`/ORM class declares, if it declares one.
    // A pydantic model or dataclass: its annotated attributes (own, after those of base
    // classes defined in this file) are the keys its `model_dump()`/`asdict()` produces.
    pub(super) fn register_record_class(&mut self, class_def: &ast::StmtClassDef) {
        let is_model = class_def.bases().iter().any(|base| match base {
            Expr::Name(n) => {
                n.id.as_str() == "BaseModel" || self.record_fields.contains_key(n.id.as_str())
            }
            Expr::Attribute(a) => a.attr.as_str() == "BaseModel",
            _ => false,
        });
        let is_dataclass = class_def.decorator_list.iter().any(|d| {
            let target = match &d.expression {
                Expr::Call(call) => &*call.func,
                other => other,
            };
            match target {
                Expr::Name(n) => n.id.as_str() == "dataclass",
                Expr::Attribute(a) => a.attr.as_str() == "dataclass",
                _ => false,
            }
        });
        if !is_model && !is_dataclass {
            return;
        }
        let mut fields: Vec<String> = Vec::new();
        for base in class_def.bases() {
            if let Expr::Name(n) = base {
                if let Some(inherited) = self.record_fields.get(n.id.as_str()) {
                    fields.extend(inherited.iter().cloned());
                }
            }
        }
        for stmt in &class_def.body {
            let Stmt::AnnAssign(ann) = stmt else {
                continue;
            };
            let Expr::Name(target) = &*ann.target else {
                continue;
            };
            let is_class_var = match &*ann.annotation {
                Expr::Subscript(sub) => {
                    matches!(&*sub.value, Expr::Name(n) if n.id.as_str() == "ClassVar")
                }
                Expr::Name(n) => n.id.as_str() == "ClassVar",
                _ => false,
            };
            let name = target.id.to_string();
            if !is_class_var && !fields.contains(&name) {
                fields.push(name);
            }
        }
        self.record_fields
            .insert(class_def.name.to_string(), fields);
    }

    // `[txn.model_dump()]` / `[asdict(txn)]` where `txn` is a parameter annotated with a
    // record class of this file: the one row's columns are the class's fields.
    pub(super) fn record_row_columns(&self, list: &ast::ExprList) -> Option<Vec<String>> {
        let [Expr::Call(call)] = list.elts.as_slice() else {
            return None;
        };
        if !call.arguments.keywords.is_empty() {
            return None;
        }
        let record = match (&*call.func, &call.arguments.args[..]) {
            (Expr::Attribute(a), []) if matches!(a.attr.as_str(), "model_dump" | "dict") => {
                &*a.value
            }
            (Expr::Name(f), [arg]) if f.id.as_str() == "asdict" => arg,
            (Expr::Attribute(a), [arg]) if a.attr.as_str() == "asdict" => arg,
            _ => return None,
        };
        let Expr::Name(record) = record else {
            return None;
        };
        let class = self.record_params.get(record.id.as_str())?;
        self.record_fields.get(class).cloned()
    }

    pub(super) fn register_schema_class(
        &mut self,
        class_def: &ast::StmtClassDef,
        errors: &mut Vec<LintError>,
    ) {
        let (class_def_line, _) = self.source_location(class_def.range().start());
        let is_schema = class_def.bases().iter().any(|base| match base {
            Expr::Attribute(attr) => ast_extract::is_schema_base(attr.attr.as_str()),
            Expr::Name(name) => {
                ast_extract::is_schema_base(name.id.as_str())
                    || self.schemas.contains_key(name.id.as_str())
            }
            _ => false,
        });

        if is_schema {
            // Collect inherited columns first (multiple-inheritance support).
            // Each named base that is already registered as a schema contributes
            // its columns; later bases can shadow earlier ones by appending, but
            // duplicate column names are left for the schema author to resolve.
            self.register_schema_class_collect_inherited_columns(class_def, errors, class_def_line);
        } else if ast_extract::class_body_has_tablename(class_def) {
            // SQLAlchemy declarative model: `class Order(Base): __tablename__ =
            // "orders"; ...`. Detected structurally, via `__tablename__` in the
            // class's own body, rather than by base-class name — the declarative
            // base is normally imported from a project-local module
            // (`class Order(Base)`), so it's never one of the fixed names
            // `is_schema_base` recognizes. Uses a separate, allowlist-based
            // extractor rather than the permissive one above: see
            // `extract_orm_columns` for why (its "any annotated attribute is a
            // column" fallback would sweep in `relationship(...)` attributes,
            // `__table_args__`, etc., which aren't real database columns).
            let columns = ast_extract::extract_orm_columns(class_def);
            // Deliberately no RESERVED_METHODS conflict check here: that
            // warning is authoring advice for typedframes-native schemas
            // ("rename the column"), but a mapped class's column names come
            // from an external database schema the user doesn't control.
            self.schemas.insert(class_def.name.to_string(), columns);
            self.schema_locations.insert(
                class_def.name.to_string(),
                (self.file_display.clone(), class_def_line),
            );
        }
    }

    fn register_schema_class_collect_inherited_columns(
        &mut self,
        class_def: &ruff_python_ast::StmtClassDef,
        errors: &mut Vec<LintError>,
        class_def_line: usize,
    ) {
        let mut columns = Vec::new();
        for base in class_def.bases() {
            if let Expr::Name(name) = base {
                if let Some(parent_cols) = self.schemas.get(name.id.as_str()) {
                    columns.extend(parent_cols.clone());
                }
            }
        }
        // Walk the class body to extract column definitions.
        // Three declaration forms are supported:
        //   1. `col: Column(...)` / `col = Column(...)` — explicit column,
        //      may have an `alias=` keyword that overrides the attribute name.
        //   2. `col: ColumnSet(members=[...])` — a named group that also
        //      expands its member strings as individual columns.
        //   3. Any other annotated attribute — treated as a plain column
        //      whose name equals the attribute name.
        collect_class_columns(class_def, &mut columns);
        // Warn about column names that conflict with reserved methods
        for col_name in &columns {
            if RESERVED_METHODS.contains(&col_name.as_str()) {
                let (line, col) = self.source_location(class_def.range().start());
                errors.push(LintError {
                        line,
                        col,
                        code: CODE_RESERVED_NAME.to_string(),
                        message: format!(
                            "Column name '{}' in {} conflicts with a pandas/polars method. This will shadow the method when accessed via attribute syntax (df.{}). Consider renaming to '{}_value' or similar.",
                            col_name, class_def.name, col_name, col_name
                        ),
                        severity: "error".to_string(),
                    });
            }
        }
        self.schemas.insert(class_def.name.to_string(), columns);
        self.schema_locations.insert(
            class_def.name.to_string(),
            (self.file_display.clone(), class_def_line),
        );
    }
}

fn collect_class_columns(class_def: &ruff_python_ast::StmtClassDef, columns: &mut Vec<String>) {
    for body_stmt in &class_def.body {
        collect_member_columns(columns, body_stmt);
    }
    // Deduplicate columns (MI may bring overlapping columns)
    columns.sort();
    columns.dedup();
}

fn collect_member_columns(columns: &mut Vec<String>, body_stmt: &Stmt) {
    if let Stmt::AnnAssign(ann_assign) = body_stmt {
        collect_annotated_column(columns, ann_assign);
    } else if let Stmt::Assign(assign) = body_stmt {
        collect_assigned_column(columns, assign);
    }
}

fn collect_annotated_column(
    columns: &mut Vec<String>,
    ann_assign: &ruff_python_ast::StmtAnnAssign,
) {
    if let Expr::Name(name) = ann_assign.target.as_ref() {
        annotated_column(columns, ann_assign, name);
    }
}

fn annotated_column(
    columns: &mut Vec<String>,
    ann_assign: &ruff_python_ast::StmtAnnAssign,
    name: &ruff_python_ast::ExprName,
) {
    let mut col_added = false;
    if let Some(value) = &ann_assign.value {
        if let Expr::Call(call) = &**value {
            annotated_column_call(columns, name, &mut col_added, call);
        }
    }
    if !col_added {
        columns.push(name.id.to_string());
    }
}

fn annotated_column_call(
    columns: &mut Vec<String>,
    name: &ruff_python_ast::ExprName,
    col_added: &mut bool,
    call: &ruff_python_ast::ExprCall,
) {
    let func_name = match &*call.func {
        Expr::Name(n) => Some(n.id.as_str()),
        Expr::Attribute(a) => Some(a.attr.as_str()),
        _ => None,
    };

    if let Some(f) = func_name {
        annotated_column_constructor(columns, name, col_added, call, f);
    }
}

fn annotated_column_constructor(
    columns: &mut Vec<String>,
    name: &ruff_python_ast::ExprName,
    col_added: &mut bool,
    call: &ruff_python_ast::ExprCall,
    f: &str,
) {
    if f == "Column" {
        annotated_column_alias(columns, name, col_added, call);
    } else if f == "ColumnSet" || f == "ColumnGroup" {
        annotated_column_set(columns, name, col_added, call);
    }
}

fn annotated_column_set(
    columns: &mut Vec<String>,
    name: &ruff_python_ast::ExprName,
    col_added: &mut bool,
    call: &ruff_python_ast::ExprCall,
) {
    columns.push(name.id.to_string());
    for keyword in call.arguments.keywords.iter() {
        annotated_column_members(columns, keyword);
    }
    *col_added = true;
}

fn annotated_column_members(columns: &mut Vec<String>, keyword: &ruff_python_ast::Keyword) {
    if keyword.arg.as_ref().map(|s| s.as_str()) == Some("members") {
        if let Expr::List(list) = &keyword.value {
            for el in &list.elts {
                if let Some(s) = ast_extract::extract_string_literal(el) {
                    columns.push(s.to_string());
                } else if let Expr::Name(n) = el {
                    columns.push(n.id.to_string());
                }
            }
        }
    }
}

fn annotated_column_alias(
    columns: &mut Vec<String>,
    name: &ruff_python_ast::ExprName,
    col_added: &mut bool,
    call: &ruff_python_ast::ExprCall,
) {
    let mut alias = None;
    for keyword in call.arguments.keywords.iter() {
        if keyword.arg.as_ref().map(|s| s.as_str()) == Some("alias") {
            if let Some(s) = ast_extract::extract_string_literal(&keyword.value) {
                alias = Some(s.to_string());
            }
        }
    }
    let col_name = alias.unwrap_or_else(|| name.id.to_string());
    columns.push(col_name);
    *col_added = true;
}

fn collect_assigned_column(columns: &mut Vec<String>, assign: &ruff_python_ast::StmtAssign) {
    for target in &assign.targets {
        collect_assigned_target(columns, assign, target);
    }
}

fn collect_assigned_target(
    columns: &mut Vec<String>,
    assign: &ruff_python_ast::StmtAssign,
    target: &Expr,
) {
    if let Expr::Name(name) = target {
        assigned_column(columns, assign, name);
    }
}

fn assigned_column(
    columns: &mut Vec<String>,
    assign: &ruff_python_ast::StmtAssign,
    name: &ruff_python_ast::ExprName,
) {
    let mut col_added = false;
    if let Expr::Call(call) = &*assign.value {
        assigned_column_call(columns, name, &mut col_added, call);
    }
    if !col_added {
        columns.push(name.id.to_string());
    }
}

fn assigned_column_call(
    columns: &mut Vec<String>,
    name: &ruff_python_ast::ExprName,
    col_added: &mut bool,
    call: &ruff_python_ast::ExprCall,
) {
    let func_name = match &*call.func {
        Expr::Name(n) => Some(n.id.as_str()),
        Expr::Attribute(a) => Some(a.attr.as_str()),
        _ => None,
    };

    if let Some(f) = func_name {
        assigned_column_constructor(columns, name, col_added, call, f);
    }
}

fn assigned_column_constructor(
    columns: &mut Vec<String>,
    name: &ruff_python_ast::ExprName,
    col_added: &mut bool,
    call: &ruff_python_ast::ExprCall,
    f: &str,
) {
    if f == "Column" {
        assigned_column_alias(columns, name, col_added, call);
    } else if f == "ColumnSet" || f == "ColumnGroup" {
        assigned_column_set(columns, name, col_added, call);
    }
}

fn assigned_column_set(
    columns: &mut Vec<String>,
    name: &ruff_python_ast::ExprName,
    col_added: &mut bool,
    call: &ruff_python_ast::ExprCall,
) {
    columns.push(name.id.to_string());
    for keyword in call.arguments.keywords.iter() {
        assigned_column_members(columns, keyword);
    }
    *col_added = true;
}

fn assigned_column_members(columns: &mut Vec<String>, keyword: &ruff_python_ast::Keyword) {
    if keyword.arg.as_ref().map(|s| s.as_str()) == Some("members") {
        if let Expr::List(list) = &keyword.value {
            for el in &list.elts {
                if let Some(s) = ast_extract::extract_string_literal(el) {
                    columns.push(s.to_string());
                } else if let Expr::Name(n) = el {
                    columns.push(n.id.to_string());
                }
            }
        }
    }
}

fn assigned_column_alias(
    columns: &mut Vec<String>,
    name: &ruff_python_ast::ExprName,
    col_added: &mut bool,
    call: &ruff_python_ast::ExprCall,
) {
    let mut alias = None;
    for keyword in call.arguments.keywords.iter() {
        if keyword.arg.as_ref().map(|s| s.as_str()) == Some("alias") {
            if let Some(s) = ast_extract::extract_string_literal(&keyword.value) {
                alias = Some(s.to_string());
            }
        }
    }
    columns.push(alias.unwrap_or_else(|| name.id.to_string()));
    *col_added = true;
}
