use super::*;

// Collect `self.<attr> = <ClassName>(...)` / `self.<attr> = <module>.<ClassName>(...)`
// assignments from `__init__`'s body into `out` -- see `Linter::current_self_attrs`'s
// doc comment for why this is what makes `self.<attr>.<method>(...)` resolvable at
// all. Descends into `if`/`for`/`while`/`with` bodies, matching `find_returned_var`'s
// recursion -- deliberately NOT into `try`/`except` or comprehensions, the same
// conservative-not-exhaustive boundary already documented for contract inference
// elsewhere in this file. Only the first assignment to a given attr name wins (a
// later reassignment is silently ignored), matching this checker's existing
// single-binding conventions (e.g. `string_var_candidates`).
// The class a type annotation names, in the `SelfAttrOrigin` shape: `Store` or `ports.Store`
// (also quoted). Anything richer (`Optional[Store]`, a union) names no single class.
fn annotation_origin(annotation: &Expr) -> Option<SelfAttrOrigin> {
    match annotation {
        Expr::Name(name) => Some(SelfAttrOrigin {
            class_name: name.id.to_string(),
            resolve_name: name.id.to_string(),
            is_attribute_call: false,
        }),
        Expr::Attribute(attr) => match &*attr.value {
            Expr::Name(module) => Some(SelfAttrOrigin {
                class_name: attr.attr.to_string(),
                resolve_name: module.id.to_string(),
                is_attribute_call: true,
            }),
            _ => None,
        },
        Expr::StringLiteral(text) => {
            let text = text.value.to_str();
            text.chars()
                .all(|c| c.is_alphanumeric() || c == '_')
                .then(|| SelfAttrOrigin {
                    class_name: text.to_string(),
                    resolve_name: text.to_string(),
                    is_attribute_call: false,
                })
        }
        _ => None,
    }
}

// The annotated parameters of a function, by name, as the class each annotation names.
pub(crate) fn annotated_param_origins(
    func_def: &ast::StmtFunctionDef,
) -> HashMap<String, SelfAttrOrigin> {
    let params = &func_def.parameters;
    params
        .posonlyargs
        .iter()
        .chain(&params.args)
        .chain(&params.kwonlyargs)
        .filter_map(|p| {
            let origin = annotation_origin(p.parameter.annotation.as_deref()?)?;
            Some((p.parameter.name.to_string(), origin))
        })
        .collect()
}

// Collect `self.<attr>` origins from `__init__`: constructor calls, and a typed parameter
// stored as is (`self.store = store` with `store: Store`) -- the dependency-injection shape.
pub(crate) fn collect_self_attr_origins(
    stmts: &[Stmt],
    params: &HashMap<String, SelfAttrOrigin>,
    out: &mut HashMap<String, SelfAttrOrigin>,
) {
    for stmt in stmts {
        match stmt {
            Stmt::Assign(assign) if assign.targets.len() == 1 => {
                let Expr::Attribute(target_attr) = &assign.targets[0] else {
                    continue;
                };
                let Expr::Name(recv) = &*target_attr.value else {
                    continue;
                };
                if recv.id.as_str() != "self" {
                    continue;
                }
                let attr_name = target_attr.attr.to_string();
                if out.contains_key(&attr_name) {
                    continue;
                }
                if let Expr::Name(param) = &*assign.value {
                    if let Some(origin) = params.get(param.id.as_str()) {
                        out.insert(attr_name, origin.clone());
                    }
                    continue;
                }
                let Expr::Call(call) = &*assign.value else {
                    continue;
                };
                match &*call.func {
                    Expr::Name(class_name) => {
                        out.insert(
                            attr_name,
                            SelfAttrOrigin {
                                class_name: class_name.id.to_string(),
                                resolve_name: class_name.id.to_string(),
                                is_attribute_call: false,
                            },
                        );
                    }
                    Expr::Attribute(class_attr) => {
                        if let Expr::Name(module) = &*class_attr.value {
                            out.insert(
                                attr_name,
                                SelfAttrOrigin {
                                    class_name: class_attr.attr.to_string(),
                                    resolve_name: module.id.to_string(),
                                    is_attribute_call: true,
                                },
                            );
                        }
                    }
                    _ => {}
                }
            }
            Stmt::If(if_stmt) => {
                collect_self_attr_origins(&if_stmt.body, params, out);
                for clause in &if_stmt.elif_else_clauses {
                    collect_self_attr_origins(&clause.body, params, out);
                }
            }
            Stmt::For(for_stmt) => collect_self_attr_origins(&for_stmt.body, params, out),
            Stmt::While(while_stmt) => collect_self_attr_origins(&while_stmt.body, params, out),
            Stmt::With(with_stmt) => collect_self_attr_origins(&with_stmt.body, params, out),
            _ => {}
        }
    }
}
