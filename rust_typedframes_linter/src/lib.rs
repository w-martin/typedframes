use pyo3::prelude::*;
use rustpython_ast::{self as ast, Expr, Stmt};
use rustpython_parser::{parse, source_code::LineIndex, Mode};
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[pyfunction]
fn check_file(file_path: String) -> PyResult<String> {
    let path = Path::new(&file_path);
    let project_root = find_project_root(path);

    if !is_enabled(&project_root) {
        return Ok("[]".to_string());
    }

    let source = fs::read_to_string(path)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyIOError, _>(format!("{}", e)))?;

    let mut linter = Linter::new();
    let errors = linter
        .check_file_internal(&source, path)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("{}", e)))?;

    serde_json::to_string(&errors)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("{}", e)))
}

#[pymodule]
fn rust_pandas_linter(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(check_file, m)?)?;
    Ok(())
}

#[derive(serde::Deserialize)]
struct Config {
    tool: Option<ToolConfig>,
}

#[derive(serde::Deserialize)]
struct ToolConfig {
    pandas_column_linter: Option<LinterConfig>,
}

#[derive(serde::Deserialize)]
struct LinterConfig {
    enabled: Option<bool>,
}

pub fn is_enabled(project_root: &Path) -> bool {
    let config_path = project_root.join("pyproject.toml");
    if !config_path.exists() {
        return true;
    }

    let content = match fs::read_to_string(config_path) {
        Ok(c) => c,
        Err(_) => return true,
    };

    let config: Config = match toml::from_str(&content) {
        Ok(c) => c,
        Err(_) => return true,
    };

    config
        .tool
        .and_then(|t| t.pandas_column_linter)
        .and_then(|l| l.enabled)
        .unwrap_or(true)
}

pub fn find_project_root(start_path: &Path) -> PathBuf {
    let mut current = start_path.to_path_buf();
    if current.is_file() {
        current.pop();
    }
    loop {
        if current.join("pyproject.toml").exists() {
            return current;
        }
        if !current.pop() {
            return start_path.to_path_buf();
        }
    }
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let a_len = a_chars.len();
    let b_len = b_chars.len();
    let mut matrix = vec![vec![0; b_len + 1]; a_len + 1];

    for i in 0..=a_len {
        matrix[i][0] = i;
    }
    for j in 0..=b_len {
        matrix[0][j] = j;
    }

    for i in 1..=a_len {
        for j in 1..=b_len {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            matrix[i][j] = std::cmp::min(
                std::cmp::min(matrix[i - 1][j] + 1, matrix[i][j - 1] + 1),
                matrix[i - 1][j - 1] + cost,
            );
        }
    }
    matrix[a_len][b_len]
}

fn find_best_match<'a>(name: &str, candidates: &'a [String]) -> Option<&'a str> {
    candidates
        .iter()
        .map(|c| (c, levenshtein(name, c)))
        .filter(|(_, dist)| *dist <= 2)
        .min_by_key(|(_, dist)| *dist)
        .map(|(c, _)| c.as_str())
}

#[derive(Debug, Serialize, PartialEq)]
pub struct LintError {
    pub line: usize,
    pub col: usize,
    pub message: String,
}

pub struct Linter {
    schemas: HashMap<String, Vec<String>>,
    variables: HashMap<String, (String, usize)>, // var_name -> (schema_name, defined_at_line)
    line_index: Option<LineIndex>,
    source: String,
}

impl Linter {
    pub fn new() -> Self {
        Self {
            schemas: HashMap::new(),
            variables: HashMap::new(),
            line_index: None,
            source: String::new(),
        }
    }

    pub fn check_file_internal(
        &mut self,
        source: &str,
        path: &Path,
    ) -> Result<Vec<LintError>, anyhow::Error> {
        self.source = source.to_string();
        self.line_index = Some(LineIndex::from_source_text(source));
        let parsed = parse(source, Mode::Module, &path.to_string_lossy())?;
        let mut errors = Vec::new();

        if let ast::Mod::Module(m) = parsed {
            for stmt in m.body {
                self.visit_stmt(&stmt, &mut errors);
            }
        }

        Ok(errors)
    }

    fn visit_stmt(&mut self, stmt: &Stmt, errors: &mut Vec<LintError>) {
        match stmt {
            Stmt::ClassDef(class_def) => {
                let is_schema = class_def.bases.iter().any(|base| match base {
                    Expr::Attribute(attr) => {
                        let attr_name = attr.attr.as_str();
                        attr_name == "DataFrameModel"
                            || attr_name == "TypedDict"
                            || attr_name == "DataFrame"
                            || attr_name == "BaseFrame"
                    }
                    Expr::Name(name) => {
                        let name_id = name.id.as_str();
                        name_id == "DataFrameModel"
                            || name_id == "TypedDict"
                            || name_id == "DataFrame"
                            || name_id == "BaseFrame"
                    }
                    _ => false,
                });

                if is_schema {
                    let mut columns = Vec::new();
                    for body_stmt in &class_def.body {
                        if let Stmt::AnnAssign(ann_assign) = body_stmt {
                            if let Expr::Name(name) = ann_assign.target.as_ref() {
                                let mut col_added = false;
                                if let Some(value) = &ann_assign.value {
                                    if let Expr::Call(call) = &**value {
                                        let func_name = match &*call.func {
                                            Expr::Name(n) => Some(n.id.as_str()),
                                            Expr::Attribute(a) => Some(a.attr.as_str()),
                                            _ => None,
                                        };

                                        if let Some(f) = func_name {
                                            if f == "Column" {
                                                let mut alias = None;
                                                for keyword in &call.keywords {
                                                    if keyword.arg.as_ref().map(|s| s.as_str())
                                                        == Some("alias")
                                                    {
                                                        if let Expr::Constant(c) = &keyword.value {
                                                            if let ast::Constant::Str(s) = &c.value
                                                            {
                                                                alias = Some(s.clone());
                                                            }
                                                        } else if let Expr::Name(n) = &keyword.value
                                                        {
                                                            if n.id.as_str() == "DefinedLater" {
                                                                // Use attribute name if it's DefinedLater for now
                                                                alias = Some(name.id.to_string());
                                                            }
                                                        }
                                                    }
                                                }
                                                let col_name =
                                                    alias.unwrap_or_else(|| name.id.to_string());
                                                columns.push(col_name);
                                                col_added = true;
                                            } else if f == "ColumnSet" || f == "ColumnGroup" {
                                                // Add the attribute itself too
                                                columns.push(name.id.to_string());
                                                for keyword in &call.keywords {
                                                    if keyword.arg.as_ref().map(|s| s.as_str())
                                                        == Some("members")
                                                    {
                                                        if let Expr::List(list) = &keyword.value {
                                                            for el in &list.elts {
                                                                if let Expr::Constant(c) = el {
                                                                    if let ast::Constant::Str(s) =
                                                                        &c.value
                                                                    {
                                                                        columns.push(s.clone());
                                                                    }
                                                                } else if let Expr::Name(n) = el {
                                                                    // For ColumnGroup, members might be other Columns
                                                                    columns.push(n.id.to_string());
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                                col_added = true;
                                            }
                                        }
                                    }
                                }
                                if !col_added {
                                    columns.push(name.id.to_string());
                                }
                            }
                        } else if let Stmt::Assign(assign) = body_stmt {
                            for target in &assign.targets {
                                if let Expr::Name(name) = target {
                                    let mut col_added = false;
                                    if let Expr::Call(call) = &*assign.value {
                                        let func_name = match &*call.func {
                                            Expr::Name(n) => Some(n.id.as_str()),
                                            Expr::Attribute(a) => Some(a.attr.as_str()),
                                            _ => None,
                                        };

                                        if let Some(f) = func_name {
                                            if f == "Column" {
                                                let mut alias = None;
                                                for keyword in &call.keywords {
                                                    if keyword.arg.as_ref().map(|s| s.as_str())
                                                        == Some("alias")
                                                    {
                                                        if let Expr::Constant(c) = &keyword.value {
                                                            if let ast::Constant::Str(s) = &c.value
                                                            {
                                                                alias = Some(s.clone());
                                                            }
                                                        } else if let Expr::Name(n) = &keyword.value
                                                        {
                                                            if n.id.as_str() == "DefinedLater" {
                                                                alias = Some(name.id.to_string());
                                                            }
                                                        }
                                                    }
                                                }
                                                columns.push(
                                                    alias.unwrap_or_else(|| name.id.to_string()),
                                                );
                                                col_added = true;
                                            } else if f == "ColumnSet" || f == "ColumnGroup" {
                                                columns.push(name.id.to_string());
                                                for keyword in &call.keywords {
                                                    if keyword.arg.as_ref().map(|s| s.as_str())
                                                        == Some("members")
                                                    {
                                                        if let Expr::List(list) = &keyword.value {
                                                            for el in &list.elts {
                                                                if let Expr::Constant(c) = el {
                                                                    if let ast::Constant::Str(s) =
                                                                        &c.value
                                                                    {
                                                                        columns.push(s.clone());
                                                                    }
                                                                } else if let Expr::Name(n) = el {
                                                                    columns.push(n.id.to_string());
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                                col_added = true;
                                            }
                                        }
                                    }
                                    if !col_added {
                                        columns.push(name.id.to_string());
                                    }
                                }
                            }
                        }
                    }
                    self.schemas.insert(class_def.name.to_string(), columns);
                }
            }
            Stmt::FunctionDef(func_def) => {
                for body_stmt in &func_def.body {
                    self.visit_stmt(body_stmt, errors);
                }
            }
            Stmt::Assign(assign) => {
                let source_location = self
                    .line_index
                    .as_ref()
                    .unwrap()
                    .source_location(assign.range.start(), &self.source);
                let current_line = source_location.row.get() as usize;

                // Check for mutations: df["new_col"] = ...
                for target in &assign.targets {
                    if let Expr::Subscript(subscript) = target {
                        if let Expr::Name(name) = &*subscript.value {
                            if let Some((schema_name, _)) = self.variables.get(name.id.as_str()) {
                                if let Expr::Constant(c) = &*subscript.slice {
                                    if let ast::Constant::Str(col_name) = &c.value {
                                        if let Some(columns) = self.schemas.get_mut(schema_name) {
                                            if !columns.contains(col_name) {
                                                // Mutation: adding a column not in schema
                                                errors.push(LintError {
                                                    line: current_line,
                                                    col: source_location.column.get() as usize,
                                                    message: format!("Column '{}' does not exist in {} (mutation tracking)", col_name, schema_name),
                                                });
                                                // For now, let's also add it to columns to avoid multiple errors if it's used later
                                                columns.push(col_name.clone());
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                if let Expr::Call(call) = &*assign.value {
                    let mut is_merge_or_concat = false;
                    let mut merge_schema = None;

                    match &*call.func {
                        Expr::Attribute(attr) => {
                            let func_name = attr.attr.as_str();
                            if func_name == "merge" {
                                if let Expr::Name(left_name) = &*attr.value {
                                    if let Some((left_schema, _)) =
                                        self.variables.get(left_name.id.as_str())
                                    {
                                        if !call.args.is_empty() {
                                            if let Expr::Name(right_name) = &call.args[0] {
                                                if let Some((right_schema, _)) =
                                                    self.variables.get(right_name.id.as_str())
                                                {
                                                    is_merge_or_concat = true;
                                                    merge_schema = Some((
                                                        left_schema.clone(),
                                                        right_schema.clone(),
                                                    ));
                                                }
                                            }
                                        }
                                    }
                                }
                            } else if func_name == "concat" {
                                // pd.concat style
                                if !call.args.is_empty() {
                                    if let Expr::List(list) = &call.args[0] {
                                        let mut schemas = Vec::new();
                                        for el in &list.elts {
                                            if let Expr::Name(n) = el {
                                                if let Some((s, _)) =
                                                    self.variables.get(n.id.as_str())
                                                {
                                                    schemas.push(s.clone());
                                                }
                                            }
                                        }
                                        if schemas.len() >= 2 {
                                            is_merge_or_concat = true;
                                            merge_schema =
                                                Some((schemas[0].clone(), schemas[1].clone()));
                                        }
                                    }
                                }
                            }
                        }
                        Expr::Name(name) => {
                            if name.id.as_str() == "concat" {
                                if !call.args.is_empty() {
                                    if let Expr::List(list) = &call.args[0] {
                                        let mut schemas = Vec::new();
                                        for el in &list.elts {
                                            if let Expr::Name(n) = el {
                                                if let Some((s, _)) =
                                                    self.variables.get(n.id.as_str())
                                                {
                                                    schemas.push(s.clone());
                                                }
                                            }
                                        }
                                        if schemas.len() >= 2 {
                                            is_merge_or_concat = true;
                                            merge_schema =
                                                Some((schemas[0].clone(), schemas[1].clone()));
                                        }
                                    }
                                } else if let Some(keyword) = call
                                    .keywords
                                    .iter()
                                    .find(|k| k.arg.as_ref().map(|s| s.as_str()) == Some("objs"))
                                {
                                    if let Expr::List(list) = &keyword.value {
                                        let mut schemas = Vec::new();
                                        for el in &list.elts {
                                            if let Expr::Name(n) = el {
                                                if let Some((s, _)) =
                                                    self.variables.get(n.id.as_str())
                                                {
                                                    schemas.push(s.clone());
                                                }
                                            }
                                        }
                                        if schemas.len() >= 2 {
                                            is_merge_or_concat = true;
                                            merge_schema =
                                                Some((schemas[0].clone(), schemas[1].clone()));
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }

                    if is_merge_or_concat {
                        if let Some((s1, s2)) = merge_schema {
                            let mut combined_cols = Vec::new();
                            if let Some(cols1) = self.schemas.get(&s1) {
                                combined_cols.extend(cols1.clone());
                            }
                            if let Some(cols2) = self.schemas.get(&s2) {
                                combined_cols.extend(cols2.clone());
                            }
                            combined_cols.sort();
                            combined_cols.dedup();

                            let combined_schema_name = format!("{}_{}", s1, s2);
                            self.schemas
                                .insert(combined_schema_name.clone(), combined_cols);
                            for target in &assign.targets {
                                if let Expr::Name(target_name) = target {
                                    self.variables.insert(
                                        target_name.id.to_string(),
                                        (combined_schema_name.clone(), current_line),
                                    );
                                }
                            }
                        }
                    }

                    let mut is_load_users = false;
                    match &*call.func {
                        Expr::Name(name) => {
                            if name.id.as_str() == "load_users" {
                                is_load_users = true;
                            }
                        }
                        Expr::Attribute(attr) => {
                            if attr.attr.as_str() == "load_users" {
                                is_load_users = true;
                            }
                        }
                        _ => {}
                    }

                    if is_load_users {
                        if let Expr::Name(target_name) = &assign.targets[0] {
                            self.variables.insert(
                                target_name.id.to_string(),
                                ("UserSchema".to_string(), current_line),
                            );
                        }
                    }

                    // Support for direct instantiation: df = DataFrame[Schema](...)
                    // or pandandic style: df = Schema().read_csv(...)
                    if let Expr::Subscript(subscript) = &*call.func {
                        if let Expr::Name(name) = &*subscript.value {
                            if name.id.as_str() == "DataFrame" {
                                if let Expr::Name(schema_name) = &*subscript.slice {
                                    for target in &assign.targets {
                                        if let Expr::Name(target_name) = target {
                                            self.variables.insert(
                                                target_name.id.to_string(),
                                                (schema_name.id.to_string(), current_line),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    } else if let Expr::Attribute(attr) = &*call.func {
                        // Handle Schema().read_csv(...)
                        let current_expr = &*attr.value;
                        // Unroll potential chains or just look for the base call
                        if let Expr::Call(inner_call) = current_expr {
                            if let Expr::Name(schema_name) = &*inner_call.func {
                                if self.schemas.contains_key(schema_name.id.as_str()) {
                                    for target in &assign.targets {
                                        if let Expr::Name(target_name) = target {
                                            self.variables.insert(
                                                target_name.id.to_string(),
                                                (schema_name.id.to_string(), current_line),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                for target in &assign.targets {
                    self.visit_expr(target, errors);
                }
            }
            Stmt::AnnAssign(ann_assign) => {
                let source_location = self
                    .line_index
                    .as_ref()
                    .unwrap()
                    .source_location(ann_assign.range.start(), &self.source);
                let current_line = source_location.row.get() as usize;

                if let Some(value) = &ann_assign.value {
                    if let Expr::Call(call) = &**value {
                        if let Expr::Subscript(subscript) = &*call.func {
                            if let Expr::Name(name) = &*subscript.value {
                                if name.id.as_str() == "DataFrame" {
                                    if let Expr::Name(schema_name) = &*subscript.slice {
                                        if let Expr::Name(target_name) = &*ann_assign.target {
                                            self.variables.insert(
                                                target_name.id.to_string(),
                                                (schema_name.id.to_string(), current_line),
                                            );
                                        }
                                    }
                                }
                            }
                        } else if let Expr::Attribute(attr) = &*call.func {
                            // Handle Schema().read_csv(...) in AnnAssign
                            let current_expr = &*attr.value;
                            if let Expr::Call(inner_call) = current_expr {
                                if let Expr::Name(schema_name) = &*inner_call.func {
                                    if self.schemas.contains_key(schema_name.id.as_str()) {
                                        if let Expr::Name(target_name) = &*ann_assign.target {
                                            self.variables.insert(
                                                target_name.id.to_string(),
                                                (schema_name.id.to_string(), current_line),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Track mutations in AnnAssign: df: DataFrame[Schema] = ...; df["new"] = ...
                // Wait, AnnAssign is for the assignment itself.
                // df["new"] = ... is always an Assign with a Subscript target in AST.
                // So no mutation tracking needed here for Subscript.

                // Track schema if explicitly hinted: df: DataFrame[UserSchema] = load()
                match &*ann_assign.annotation {
                    Expr::Subscript(subscript) => {
                        let mut is_df = false;
                        let mut name_to_use = None;
                        if let Expr::Name(name) = &*subscript.value {
                            name_to_use = Some(name.id.to_string());
                            if name.id.as_str() == "DataFrame" {
                                is_df = true;
                            }
                        } else if let Expr::Attribute(attr) = &*subscript.value {
                            name_to_use = Some(attr.attr.to_string());
                            if attr.attr.as_str() == "DataFrame" {
                                is_df = true;
                            }
                        }

                        if is_df {
                            if let Expr::Name(schema_name) = &*subscript.slice {
                                if let Expr::Name(target_name) = &*ann_assign.target {
                                    self.variables.insert(
                                        target_name.id.to_string(),
                                        (schema_name.id.to_string(), current_line),
                                    );
                                }
                            } else if let Expr::Constant(c) = &*subscript.slice {
                                // Support for quoted type hints: df: "DataFrame[UserSchema]"
                                if let ast::Constant::Str(s) = &c.value {
                                    if s.contains("DataFrame[") && s.contains(']') {
                                        if let Some(start) = s.find('[') {
                                            if let Some(end) = s.rfind(']') {
                                                let schema_name = &s[start + 1..end];
                                                if let Expr::Name(target_name) = &*ann_assign.target
                                                {
                                                    self.variables.insert(
                                                        target_name.id.to_string(),
                                                        (schema_name.to_string(), current_line),
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        } else if let Some(n) = name_to_use {
                            // Not "DataFrame", maybe it's "Annotated[DataFrame, UserSchema]" or just "UserSchema"
                            if n == "Annotated" {
                                // Handle Annotated[DataFrame, UserSchema]
                                if let Expr::Tuple(tuple) = &*subscript.slice {
                                    if tuple.elts.len() >= 2 {
                                        let mut is_df_in_annotated = false;
                                        if let Expr::Name(first) = &tuple.elts[0] {
                                            if first.id.as_str() == "DataFrame" {
                                                is_df_in_annotated = true;
                                            }
                                        }
                                        if is_df_in_annotated {
                                            if let Expr::Name(second) = &tuple.elts[1] {
                                                if let Expr::Name(target_name) = &*ann_assign.target
                                                {
                                                    self.variables.insert(
                                                        target_name.id.to_string(),
                                                        (second.id.to_string(), current_line),
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Expr::Constant(c) => {
                        // Handle quoted type hint directly on annotation: df: "DataFrame[UserSchema]"
                        if let ast::Constant::Str(s) = &c.value {
                            if s.contains("DataFrame[") && s.contains(']') {
                                if let Some(start) = s.find('[') {
                                    if let Some(end) = s.rfind(']') {
                                        let schema_name = &s[start + 1..end];
                                        if let Expr::Name(target_name) = &*ann_assign.target {
                                            self.variables.insert(
                                                target_name.id.to_string(),
                                                (schema_name.to_string(), current_line),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }

                self.visit_expr(&ann_assign.target, errors);
                if let Some(value) = &ann_assign.value {
                    self.visit_expr(value, errors);
                }
            }
            Stmt::Expr(expr_stmt) => {
                self.visit_expr(&expr_stmt.value, errors);
            }
            _ => {}
        }
    }

    fn visit_expr(&self, expr: &Expr, errors: &mut Vec<LintError>) {
        match expr {
            Expr::Attribute(attr) => {
                if let Expr::Name(name) = &*attr.value {
                    if let Some((schema_name, defined_line)) = self.variables.get(name.id.as_str())
                    {
                        if let Some(columns) = self.schemas.get(schema_name) {
                            let attr_name = attr.attr.as_str();
                            // In pandas, some attributes are valid (e.g. .shape, .columns, .index)
                            // But for pandandic/pandera, columns are also attributes.
                            let pandas_internals = [
                                "shape",
                                "columns",
                                "index",
                                "iloc",
                                "loc",
                                "head",
                                "tail",
                                "describe",
                                "info",
                                "set_index",
                                "merge",
                                "concat",
                                "join",
                            ];
                            if !columns.contains(&attr_name.to_string())
                                && !pandas_internals.contains(&attr_name)
                            {
                                let source_location = self
                                    .line_index
                                    .as_ref()
                                    .unwrap()
                                    .source_location(attr.range.start(), &self.source);
                                let mut message = format!(
                                    "Column '{}' does not exist in {} (defined at line {})",
                                    attr_name, schema_name, defined_line
                                );
                                if let Some(suggestion) = find_best_match(attr_name, columns) {
                                    message.push_str(&format!(" (did you mean '{}'?)", suggestion));
                                }
                                errors.push(LintError {
                                    line: source_location.row.get() as usize,
                                    col: source_location.column.get() as usize,
                                    message,
                                });
                            }
                        }
                    }
                }
                self.visit_expr(&attr.value, errors);
            }
            Expr::Subscript(subscript) => {
                if let Expr::Name(name) = &*subscript.value {
                    if let Some((schema_name, defined_line)) = self.variables.get(name.id.as_str())
                    {
                        if let Some(columns) = self.schemas.get(schema_name) {
                            if let Expr::Constant(c) = &*subscript.slice {
                                if let ast::Constant::Str(col_name) = &c.value {
                                    if !columns.contains(col_name) {
                                        let source_location = self
                                            .line_index
                                            .as_ref()
                                            .unwrap()
                                            .source_location(subscript.range.start(), &self.source);
                                        let mut message = format!(
                                            "Column '{}' does not exist in {} (defined at line {})",
                                            col_name, schema_name, defined_line
                                        );
                                        if let Some(suggestion) = find_best_match(col_name, columns)
                                        {
                                            message.push_str(&format!(
                                                " (did you mean '{}'?)",
                                                suggestion
                                            ));
                                        }
                                        errors.push(LintError {
                                            line: source_location.row.get() as usize,
                                            col: source_location.column.get() as usize,
                                            message,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
                self.visit_expr(&subscript.value, errors);
                self.visit_expr(&subscript.slice, errors);
            }
            Expr::Call(call) => {
                for arg in &call.args {
                    self.visit_expr(arg, errors);
                }
                self.visit_expr(&call.func, errors);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_levenshtein() {
        // arrange
        let a = "email";
        let b = "emai";

        // act
        let dist = levenshtein(a, b);

        // assert
        assert_eq!(dist, 1);
    }

    #[test]
    fn test_find_best_match() {
        // arrange
        let name = "emai";
        let candidates = vec!["user_id".to_string(), "email".to_string()];

        // act
        let result = find_best_match(name, &candidates);

        // assert
        assert_eq!(result, Some("email"));
    }
}
