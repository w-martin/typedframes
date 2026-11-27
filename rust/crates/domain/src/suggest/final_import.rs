use super::*;

pub(super) struct InsertionPoint {
    pub(super) offset: usize,
    pub(super) indent: String,
    // The statement is the last line of a file with no trailing newline.
    pub(super) leading_newline: bool,
    pub(super) newline: &'static str,
}

// The start of the line after the origin statement, indented like it. Refused when other
// code shares the origin's lines (`a = 1; df = load()`), since a new line after it would
// not directly follow the load.
pub(super) fn insertion_point(origin: &Origin) -> Result<InsertionPoint, String> {
    let source = origin.source;
    let line_start = source[..origin.stmt_start].rfind('\n').map_or(0, |i| i + 1);
    let indent = &source[line_start..origin.stmt_start];
    if !indent.chars().all(|c| c == ' ' || c == '\t') {
        return Err("it shares a line with other code".to_string());
    }
    let rest = &source[origin.stmt_end..];
    let line_len = rest.find('\n').unwrap_or(rest.len());
    let trailing = rest[..line_len].trim_start();
    if !trailing.is_empty() && !trailing.starts_with('#') {
        return Err("it shares a line with other code".to_string());
    }
    let (offset, leading_newline) = match rest.find('\n') {
        Some(i) => (origin.stmt_end + i + 1, false),
        None => (source.len(), true),
    };
    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    Ok(InsertionPoint {
        offset,
        indent: indent.to_string(),
        leading_newline,
        newline: if leading_newline { "" } else { newline },
    })
}

/// A unified diff of `old` -> `new`, or an empty string when they are equal. Assumes
/// the change is one contiguous region (true of every edit made here), found by
/// trimming the common leading and trailing lines.
/// Where `Final` comes from in a module: a name already imported, or the line to add.
pub(super) enum FinalName {
    Imported(String),
    Add(Edit),
}

// The line ending the file uses where `offset` is: `\r\n` if the line before it ends that way.
fn line_ending_at(source: &str, offset: usize) -> &'static str {
    let line_end = source[offset..]
        .find('\n')
        .map_or(source.len(), |i| offset + i);
    if source[..line_end].ends_with('\r') {
        "\r\n"
    } else {
        "\n"
    }
}

// The end of the logical line holding the offset `end`: past a trailing comment, a `;` and
// whatever follows it, and past a statement that begins on this line and continues onto the
// next. A new line inserted here is never inside another statement or between code and
// its comment.
fn logical_line_end(module: &ModModule, source: &str, end: usize) -> usize {
    let mut at = end;
    loop {
        let newline = source[at..].find('\n').map_or(source.len(), |i| at + i);
        let continued = module.body.iter().find(|stmt| {
            let range = stmt.range();
            (at..newline).contains(&range.start().to_usize()) && range.end().to_usize() > newline
        });
        match continued {
            Some(stmt) => at = stmt.range().end().to_usize(),
            None => return newline,
        }
    }
}

fn rebinds_final(module: &ModModule) -> bool {
    module.body.iter().any(|stmt| match stmt {
        Stmt::ClassDef(c) => c.name.as_str() == "Final",
        Stmt::FunctionDef(f) => f.name.as_str() == "Final",
        Stmt::ImportFrom(import) => {
            let typing = import
                .module
                .as_ref()
                .is_some_and(|m| matches!(m.as_str(), "typing" | "typing_extensions"));
            !typing
                && import
                    .names
                    .iter()
                    .any(|a| a.asname.as_ref().unwrap_or(&a.name).as_str() == "Final")
        }
        other => binds_name(other, "Final"),
    })
}

// Adds `Final` to an existing top-level `from typing import ...` instead of a second import
// line, which an import sorter (ruff's `I001`) would then reject. Placed among the names
// the way isort orders them: CamelCase names alphabetically, lowercase ones after.
// Ruff's isort order for imported members (`order-by-type`, case-insensitive): constants
// (`TYPE_CHECKING`), then classes (`Any`), then everything else, each alphabetical.
fn isort_member_key(name: &str) -> (u8, String) {
    let constant = name.chars().count() > 1
        && name.chars().any(char::is_alphabetic)
        && !name.chars().any(char::is_lowercase);
    let rank = if constant {
        0
    } else if name.chars().next().is_some_and(char::is_uppercase) {
        1
    } else {
        2
    };
    (rank, name.to_lowercase())
}

fn merge_into_typing_import(module: &ModModule) -> Option<Edit> {
    let import = module.body.iter().find_map(|stmt| match stmt {
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
    })?;
    let final_key = isort_member_key("Final");
    let before = import
        .names
        .iter()
        .find(|a| isort_member_key(a.name.as_str()) > final_key);
    let (at, replacement) = match before {
        Some(alias) => (alias.range().start().to_usize(), "Final, "),
        None => (import.names.last()?.range().end().to_usize(), ", Final"),
    };
    Some(Edit {
        start: at,
        end: at,
        replacement: replacement.to_string(),
    })
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum ImportSection {
    Future,
    Stdlib,
    Other,
}

fn import_module(stmt: &Stmt) -> String {
    match stmt {
        Stmt::Import(import) => import
            .names
            .first()
            .map_or(String::new(), |a| a.name.to_string()),
        Stmt::ImportFrom(import) => {
            let dots = ".".repeat(import.level as usize);
            format!(
                "{dots}{}",
                import.module.as_ref().map_or("", |m| m.as_str())
            )
        }
        _ => String::new(),
    }
}

fn import_section(stmt: &Stmt) -> ImportSection {
    let module = import_module(stmt);
    let top = module.split('.').next().unwrap_or("");
    if matches!(stmt, Stmt::ImportFrom(i) if i.level == 0) && top == "__future__" {
        ImportSection::Future
    } else if !module.starts_with('.') && STDLIB_MODULES.contains(&top) {
        ImportSection::Stdlib
    } else {
        ImportSection::Other
    }
}

// How an import sorter orders statements within a section: `import x` before `from x import`,
// then by module name without regard to case.
fn import_key(stmt: &Stmt) -> (u8, String) {
    (
        u8::from(matches!(stmt, Stmt::ImportFrom(_))),
        import_module(stmt).to_lowercase(),
    )
}

// The import statements opening the file, after any docstring.
fn leading_imports(module: &ModModule) -> Vec<&Stmt> {
    let is_docstring =
        |stmt: &Stmt| matches!(stmt, Stmt::Expr(e) if matches!(&*e.value, Expr::StringLiteral(_)));
    module
        .body
        .iter()
        .skip_while(|stmt| is_docstring(stmt))
        .take_while(|stmt| matches!(stmt, Stmt::Import(_) | Stmt::ImportFrom(_)))
        .collect()
}

/// Whether the file's opening imports are grouped and ordered the way an import sorter
/// (ruff's `I001`) wants them, as far as sections and the standard library go: `__future__`,
/// then the standard library, then everything else, a blank line between groups and none
/// within one.
pub(crate) fn imports_in_order(source: &str) -> bool {
    let Ok(module) = ruff_python_parser::parse_module(source) else {
        return true;
    };
    let module = module.into_syntax();
    let leading = leading_imports(&module);
    leading.windows(2).all(|pair| {
        let (before, after) = (import_section(pair[0]), import_section(pair[1]));
        let between = &source[pair[0].range().end().to_usize()..pair[1].range().start().to_usize()];
        let blank_lines = between.matches('\n').count().saturating_sub(1);
        match before.cmp(&after) {
            std::cmp::Ordering::Less => blank_lines >= 1,
            std::cmp::Ordering::Equal => match after {
                // Third-party and first-party code are groups of their own.
                ImportSection::Other => blank_lines <= 1,
                _ => blank_lines == 0 && import_key(pair[0]) <= import_key(pair[1]),
            },
            std::cmp::Ordering::Greater => false,
        }
    })
}

pub(super) fn final_name(module: &ModModule, source: &str) -> Result<FinalName, String> {
    if rebinds_final(module) {
        return Err("`Final` is already bound to something else in this file".to_string());
    }
    for stmt in &module.body {
        match stmt {
            Stmt::ImportFrom(import)
                if import.level == 0
                    && import
                        .module
                        .as_ref()
                        .is_some_and(|m| matches!(m.as_str(), "typing" | "typing_extensions")) =>
            {
                if let Some(alias) = import.names.iter().find(|a| a.name.as_str() == "Final") {
                    let name = alias.asname.as_ref().unwrap_or(&alias.name);
                    return Ok(FinalName::Imported(name.to_string()));
                }
            }
            Stmt::Import(import) => {
                if let Some(alias) = import.names.iter().find(|a| a.name.as_str() == "typing") {
                    let name = alias.asname.as_ref().map_or("typing", |a| a.as_str());
                    return Ok(FinalName::Imported(format!("{name}.Final")));
                }
            }
            _ => {}
        }
    }
    if let Some(edit) = merge_into_typing_import(module) {
        return Ok(FinalName::Add(edit));
    }
    let edit = final_import_edit(module, source);
    Ok(FinalName::Add(edit))
}

fn final_import_edit(module: &ModModule, source: &str) -> Edit {
    let is_docstring =
        |stmt: &Stmt| matches!(stmt, Stmt::Expr(e) if matches!(&*e.value, Expr::StringLiteral(_)));
    let leading = leading_imports(module);
    let line_start = |stmt: &Stmt| {
        let start = stmt.range().start().to_usize();
        source[..start].rfind('\n').map_or(0, |i| i + 1)
    };
    // Where an import sorter (ruff's `I001`) wants `from typing import Final`: inside the
    // standard-library group, by module name, or in a group of its own ahead of the rest.
    let stdlib: Vec<&Stmt> = leading
        .iter()
        .copied()
        .filter(|stmt| import_section(stmt) == ImportSection::Stdlib)
        .collect();
    let target = (1, "typing".to_string());
    let after_last = |last: &Stmt| {
        // Inserted before the line's newline, so in a notebook the import stays in
        // the cell that holds the last import.
        let at = logical_line_end(module, source, last.range().end().to_usize());
        let eol = line_ending_at(source, at.saturating_sub(1));
        let at = if source[..at].ends_with('\r') {
            at - 1
        } else {
            at
        };
        (at, eol)
    };
    let edit = if let Some(next) = stdlib.iter().find(|stmt| import_key(stmt) > target) {
        let at = line_start(next);
        let eol = line_ending_at(source, at.saturating_sub(1));
        Edit {
            start: at,
            end: at,
            replacement: format!("from typing import Final{eol}"),
        }
    } else if let Some(last) = stdlib.last() {
        let (at, eol) = after_last(last);
        Edit {
            start: at,
            end: at,
            replacement: format!("{eol}from typing import Final"),
        }
    } else if let Some(first_other) = leading
        .iter()
        .find(|stmt| import_section(stmt) != ImportSection::Future)
    {
        let at = line_start(first_other);
        let eol = line_ending_at(source, at.saturating_sub(1));
        Edit {
            start: at,
            end: at,
            replacement: format!("from typing import Final{eol}{eol}"),
        }
    } else if let Some(last) = leading.last() {
        let (at, eol) = after_last(last);
        Edit {
            start: at,
            end: at,
            replacement: format!("{eol}{eol}from typing import Final"),
        }
    } else {
        let first = module.body.iter().find(|stmt| !is_docstring(stmt));
        let at = first.map_or(source.len(), line_start);
        let eol = line_ending_at(source, at.saturating_sub(1));
        Edit {
            start: at,
            end: at,
            replacement: format!("from typing import Final{eol}{eol}"),
        }
    };
    edit
}
