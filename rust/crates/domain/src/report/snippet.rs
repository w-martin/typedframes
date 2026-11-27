use super::*;

fn tokens_of(parsed: &ruff_python_parser::Parsed<ast::ModModule>) -> Vec<Tok> {
    parsed
        .tokens()
        .iter()
        .map(|t| Tok {
            kind: format!("{:?}", t.kind()),
            start: t.range().start().to_usize(),
            end: t.range().end().to_usize(),
        })
        .collect()
}

// Library imports, kept as written.
fn redact_library_imports(redactor: &mut Redactor, body: &[Stmt]) {
    redactor.in_import = true;
    for stmt in body.iter().filter(|s| is_kept_import(s)) {
        redactor.redact(
            stmt.range().start().to_usize(),
            stmt.range().end().to_usize(),
        );
        redactor.out.push('\n');
    }
    redactor.in_import = false;
}

fn function_names(body: &[Stmt]) -> BTreeSet<&str> {
    body.iter()
        .filter_map(|s| match s {
            Stmt::FunctionDef(f) => Some(f.name.as_str()),
            _ => None,
        })
        .collect()
}

fn class_names(body: &[Stmt]) -> BTreeSet<&str> {
    body.iter()
        .filter_map(|s| match s {
            Stmt::ClassDef(c) => Some(c.name.as_str()),
            _ => None,
        })
        .collect()
}

// Signature-only stand-ins for imported functions whose library the index knew, and the
// libraries they name.
fn function_stubs(
    redactor: &mut Redactor,
    called: &BTreeSet<String>,
    defined: &BTreeSet<&str>,
    stubs: &HashMap<String, Library>,
) -> (String, BTreeSet<&'static str>) {
    let mut stub_lines = String::new();
    let mut stub_libraries: BTreeSet<&'static str> = BTreeSet::new();
    for name in called {
        if defined.contains(name.as_str()) {
            continue;
        }
        if let Some(&library) = stubs.get(name) {
            let (alias, module) = match library {
                Library::Pandas => ("pd", "pandas"),
                Library::Polars => ("pl", "polars"),
            };
            stub_libraries.insert(module);
            let renamed = redactor.namer.name(name, 'f', false);
            stub_lines.push_str(&format!(
                "def {renamed}(*args, **kwargs) -> {alias}.DataFrame: ...\n"
            ));
        }
    }
    (stub_lines, stub_libraries)
}

// Stand-ins for imported classes the kept code names, with the methods whose library the index
// knew.
fn class_stubs_for<'c>(
    class_stubs: &'c HashMap<(String, String), Library>,
    referenced: &BTreeSet<String>,
    defined_classes: &BTreeSet<&str>,
) -> BTreeMap<&'c str, Vec<(&'c str, Library)>> {
    let mut stub_classes: BTreeMap<&str, Vec<(&str, Library)>> = BTreeMap::new();
    for ((class, method), library) in class_stubs {
        if referenced.contains(class) && !defined_classes.contains(class.as_str()) {
            stub_classes
                .entry(class)
                .or_default()
                .push((method, *library));
        }
    }
    stub_classes
}

// Other imports the kept code uses, renamed, so an imported name stays imported. `taken`: names
// the snippet already defines or stands in for.
fn renamed_imports(
    redactor: &mut Redactor,
    body: &[Stmt],
    called: &BTreeSet<String>,
    referenced: &BTreeSet<String>,
    taken: &BTreeSet<&str>,
) -> String {
    let mut import_lines = String::new();
    for stmt in body.iter().filter(|s| !is_kept_import(s)) {
        let (module, aliases) = match stmt {
            Stmt::Import(i) => (None, &i.names),
            Stmt::ImportFrom(i) => match i.module.as_ref().map(ast::Identifier::as_str) {
                Some("__future__") => continue,
                module => (Some(module.unwrap_or("")), &i.names),
            },
            _ => continue,
        };
        for alias in aliases {
            let imported = alias.name.as_str();
            let bound = alias.asname.as_ref().map_or_else(
                || imported.split('.').next().unwrap_or(imported),
                |a| a.as_str(),
            );
            if imported == "*" || !referenced.contains(bound) || taken.contains(bound) {
                continue;
            }
            rename_import_alias(redactor, called, &mut import_lines, module, bound);
        }
    }
    import_lines
}

/// Redacted code around the site at `site_offset`, and where the site ended up in it.
pub(super) fn build_snippet(
    source: &str,
    site_offset: usize,
    stubs: &HashMap<String, Library>,
    class_stubs: &HashMap<(String, String), Library>,
) -> Result<(String, (usize, usize)), String> {
    let parsed = parse_module(source).map_err(|_| "the file does not parse".to_string())?;
    let tokens = tokens_of(&parsed);
    let body = &parsed.syntax().body;
    let mut kept = Kept::around(body, site_offset)?;
    kept.add_callees(body, source, &tokens);

    let mut redactor = Redactor {
        source,
        tokens: &tokens,
        namer: Namer::default(),
        out: String::new(),
        site_offset,
        site_position: None,
        refusal: None,
        in_import: false,
    };
    redact_library_imports(&mut redactor, body);

    let defined = function_names(body);
    let called = kept.called_names(body, source, &tokens);
    let (stub_lines, stub_libraries) = function_stubs(&mut redactor, &called, &defined, stubs);
    let defined_classes = class_names(body);
    let referenced = kept.referenced_names(body, source, &tokens);
    let stub_classes = class_stubs_for(class_stubs, &referenced, &defined_classes);
    let taken: BTreeSet<&str> = defined
        .iter()
        .chain(&defined_classes)
        .copied()
        .chain(stubs.keys().map(String::as_str))
        .chain(stub_classes.keys().copied())
        .collect();
    let import_lines = renamed_imports(&mut redactor, body, &called, &referenced, &taken);
    append_class_stubs(
        &mut redactor,
        stub_lines,
        stub_libraries,
        stub_classes,
        import_lines,
    );
    kept.redact_into(&mut redactor, body, source, &tokens);
    finish_snippet(redactor)
}

fn rename_import_alias(
    redactor: &mut Redactor<'_>,
    called: &BTreeSet<String>,
    import_lines: &mut String,
    module: Option<&str>,
    bound: &str,
) {
    let prefix = if called.contains(bound) {
        'f'
    } else if bound.starts_with(char::is_uppercase) {
        'C'
    } else {
        'v'
    };
    let renamed = redactor.namer.name(bound, prefix, false);
    if renamed == bound {
        return;
    }
    let line = match module {
        Some(module) => {
            let module = redactor.namer.name(&format!("module:{module}"), 'm', false);
            format!("from {module} import {renamed}\n")
        }
        None => format!("import {renamed}\n"),
    };
    if !import_lines.contains(&line) {
        import_lines.push_str(&line);
    }
}

fn finish_snippet(mut redactor: Redactor<'_>) -> Result<(String, (usize, usize)), String> {
    if let Some(reason) = redactor.refusal.take() {
        return Err(reason);
    }
    let position = redactor
        .site_position
        .ok_or("the site is not inside the code that was kept")?;
    let text: String = redactor
        .out
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    let trimmed = text.trim_start_matches('\n');
    let removed_lines = text.len() - trimmed.len();
    let snippet = format!("{trimmed}\n");
    // Leading blank lines were removed, so the recorded line moves up by that many.
    if snippet.lines().count() > MAX_UNREDUCED_LINES {
        return Err(format!(
            "the code around the site is {} lines, over the {MAX_UNREDUCED_LINES}-line limit; trim it by hand",
            snippet.lines().count()
        ));
    }
    Ok((snippet, (position.0 - removed_lines, position.1)))
}

fn append_class_stubs(
    redactor: &mut Redactor<'_>,
    mut stub_lines: String,
    mut stub_libraries: BTreeSet<&'static str>,
    stub_classes: BTreeMap<&str, Vec<(&str, Library)>>,
    import_lines: String,
) {
    for (class, mut methods) in stub_classes {
        methods.sort_by_key(|(method, _)| *method);
        let renamed = redactor.namer.name(class, 'C', false);
        stub_lines.push_str(&format!("class {renamed}:\n"));
        for (method, library) in methods {
            let (alias, module) = match library {
                Library::Pandas => ("pd", "pandas"),
                Library::Polars => ("pl", "polars"),
            };
            stub_libraries.insert(module);
            let method = redactor.namer.name(method, 'f', true);
            stub_lines.push_str(&format!(
                "    def {method}(self, *args, **kwargs) -> {alias}.DataFrame: ...\n"
            ));
        }
    }
    for module in stub_libraries {
        let alias = if module == "pandas" { "pd" } else { "pl" };
        let line = format!("import {module} as {alias}\n");
        if !redactor.out.contains(&line) {
            redactor.out.insert_str(0, &line);
        }
    }
    // The inserted imports shift the site's recorded position only if it was already
    // seen; imports come first, so nothing has been recorded yet.
    redactor.out.push_str(&import_lines);
    redactor.out.push_str(&stub_lines);
}
