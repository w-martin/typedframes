use super::*;

impl Linter {
    // The columns of an expression that evaluates to a frame, and how to describe it in a
    // message: a tracked name, or a chain whose columns follow from literal edits.
    pub(super) fn expr_frame_columns(&self, expr: &Expr) -> Option<(Vec<String>, String)> {
        if let Expr::Name(name) = expr {
            let (schema, line) = self.variables.get(name.id.as_str())?;
            if self.schema_is_unresolved(schema) {
                return None;
            }
            return Some((
                self.schemas.get(schema)?.clone(),
                self.schema_display(schema, *line),
            ));
        }
        let columns = self.returned_expr_columns(expr)?;
        let (line, _) = self.source_location(expr.range().start());
        let display = format!(
            "inferred column set {{{}}} (defined at line {line})",
            columns.join(", ")
        );
        Some((columns, display))
    }

    fn report_unknown_column(
        &self,
        name: &str,
        columns: &[String],
        display: &str,
        at: ruff_text_size::TextSize,
        errors: &mut Vec<LintError>,
    ) {
        let (line, col) = self.source_location(at);
        let mut message = format!("Column '{name}' does not exist in {display}");
        if let Some(suggestion) = find_best_match(name, columns) {
            message.push_str(&format!(" (did you mean '{suggestion}'?)"));
        }
        errors.push(LintError {
            line,
            col,
            code: CODE_UNKNOWN_COLUMN.to_string(),
            message,
            severity: "error".to_string(),
        });
    }

    // Each of `names` must be a column of the frame `frame` evaluates to. A frame whose
    // columns are not known is left alone: selecting columns off it is how it gets narrowed.
    pub(super) fn check_names_against(
        &self,
        frame: &Expr,
        names: &[String],
        at: ruff_text_size::TextSize,
        errors: &mut Vec<LintError>,
    ) {
        let Some((columns, display)) = self.expr_frame_columns(frame) else {
            return;
        };
        for name in names.iter().filter(|n| !columns.contains(n)) {
            self.report_unknown_column(name, &columns, &display, at, errors);
        }
    }

    // Column names an index-aware pandas method takes as arguments (`merge(on=)`,
    // `sort_values`, `groupby`, ...) checked against the frame, whether it is closed or only
    // known by name. A name may also be an index level, so frames that carry one are skipped.
    pub(super) fn check_column_args(
        &self,
        frame: &Expr,
        names: &[String],
        at: ruff_text_size::TextSize,
        errors: &mut Vec<LintError>,
    ) {
        if names.is_empty() || self.may_carry_index_levels(frame) {
            return;
        }
        self.check_names_against(frame, names, at, errors);
        if let Expr::Name(root) = frame {
            for name in names {
                self.check_open_frame_read(root.id.as_str(), name, at, errors);
            }
        }
        self.check_names_against_open_chain(frame, names, at, errors);
    }

    // `with_columns(expr.alias("a"), pl.col("a") * 2)`: polars evaluates the expressions against
    // the frame it was given, so a sibling's output is not there to read. On a frame whose
    // columns are unknown the input may already have the name, hence a warning.
    pub(super) fn check_sibling_reads(
        &self,
        receiver: &Expr,
        call: &ast::ExprCall,
        errors: &mut Vec<LintError>,
    ) {
        let already_has = |name: &str| match receiver {
            Expr::Name(root) => self
                .open_frames
                .get(root.id.as_str())
                .is_some_and(|open| open.written.contains(name)),
            _ => false,
        };
        let arguments: Vec<(&Expr, Option<String>)> = call
            .arguments
            .args
            .iter()
            .map(|a| (a, ast_extract::pl_expr_output_name(a)))
            .chain(
                call.arguments
                    .keywords
                    .iter()
                    .map(|k| (&k.value, k.arg.as_ref().map(|a| a.to_string()))),
            )
            .collect();
        for (i, (expr, _)) in arguments.iter().enumerate() {
            for name in ast_extract::collect_pl_col_names(expr) {
                // A sibling that overwrites `name` from `name` itself proves the input has it.
                let overwritten_in_place = arguments.iter().any(|(e, output)| {
                    output.as_deref() == Some(name.as_str())
                        && ast_extract::collect_pl_col_names(e).contains(&name)
                });
                let created_by_sibling = !overwritten_in_place
                    && !already_has(&name)
                    && arguments
                        .iter()
                        .enumerate()
                        .any(|(j, (_, output))| j != i && output.as_deref() == Some(name.as_str()));
                let created_by_itself = arguments[i].1.as_deref() == Some(name.as_str());
                if !created_by_sibling || created_by_itself {
                    continue;
                }
                let (line, col) = self.source_location(expr.range().start());
                errors.push(LintError {
                    line,
                    col,
                    code: CODE_READ_BEFORE_ASSIGNMENT.to_string(),
                    message: format!(
                        "Column '{name}' is read here, but a sibling expression of the same `with_columns` creates it; polars evaluates every expression against the frame it was called on, so this raises ColumnNotFoundError unless that frame already has '{name}'"
                    ),
                    severity: "warning".to_string(),
                });
            }
        }
    }

    // Names read off a frame known only by name, whether it is the name itself or a chain of
    // edits on it.
    pub(super) fn check_names_against_open(
        &self,
        frame: &Expr,
        names: &[String],
        at: ruff_text_size::TextSize,
        errors: &mut Vec<LintError>,
    ) {
        if let Expr::Name(root) = frame {
            for name in names {
                self.check_open_frame_read(root.id.as_str(), name, at, errors);
            }
        }
        self.check_names_against_open_chain(frame, names, at, errors);
    }

    // `frame.drop("a").select("a")` on a frame known only by name: `a` is certainly gone.
    fn check_names_against_open_chain(
        &self,
        chain: &Expr,
        names: &[String],
        at: ruff_text_size::TextSize,
        errors: &mut Vec<LintError>,
    ) {
        if matches!(chain, Expr::Name(_)) {
            return;
        }
        let (line, col) = self.source_location(at);
        let Some(open) = self.open_after(chain, line) else {
            return;
        };
        for name in names {
            if let Some((why, _)) = open.absent.get(name) {
                errors.push(LintError {
                    line,
                    col,
                    code: CODE_UNKNOWN_COLUMN.to_string(),
                    message: format!(
                        "Column '{name}' does not exist in the frame this chain returns: it was {why} (the frame's other columns are unknown)"
                    ),
                    severity: "error".to_string(),
                });
            }
        }
    }

    fn may_carry_index_levels(&self, frame: &Expr) -> bool {
        match frame {
            Expr::Name(name) => self
                .variables
                .get(name.id.as_str())
                .is_some_and(|(schema, _)| {
                    self.schema_index.contains_key(schema) || self.unknown_index.contains(schema)
                }),
            Expr::Call(call) => match &*call.func {
                // A polars `group_by(..).agg(..)`: polars frames have no index.
                Expr::Attribute(attr)
                    if matches!(attr.attr.as_str(), "agg" | "aggregate")
                        && matches!(&*attr.value, Expr::Call(g) if matches!(&*g.func,
                            Expr::Attribute(ga) if matches!(ga.attr.as_str(), "group_by" | "group_by_dynamic"))) =>
                {
                    false
                }
                Expr::Attribute(attr) => {
                    matches!(
                        attr.attr.as_str(),
                        "set_index"
                            | "agg"
                            | "aggregate"
                            | "pivot_table"
                            | "unstack"
                            | "stack"
                            | "rename_axis"
                            | "set_axis"
                    ) || self.may_carry_index_levels(&attr.value)
                }
                _ => false,
            },
            Expr::Attribute(attr) => self.may_carry_index_levels(&attr.value),
            Expr::Subscript(sub) => self.may_carry_index_levels(&sub.value),
            _ => false,
        }
    }

    // As `check_names_against`, for arguments that only mean columns when the frame's
    // columns are all known: an unresolved frame is reported by its first column read.
    pub(super) fn check_names_against_closed(
        &self,
        frame: &Expr,
        names: &[String],
        at: ruff_text_size::TextSize,
        errors: &mut Vec<LintError>,
    ) {
        if names.is_empty() {
            return;
        }
        if let Some((columns, display)) = self.expr_frame_columns(frame) {
            for name in names.iter().filter(|n| !columns.contains(n)) {
                self.report_unknown_column(name, &columns, &display, at, errors);
            }
        }
    }

    // The columns `lambda d: ...` reads off its single parameter, which is bound to a
    // frame (or, under `apply(axis=1)`, to one of its rows).
    pub(super) fn check_lambda_rows(
        &self,
        lambda: &ast::ExprLambda,
        columns: &[String],
        display: &str,
        errors: &mut Vec<LintError>,
    ) {
        let Some(parameters) = &lambda.parameters else {
            return;
        };
        let [parameter] = parameters.args.as_slice() else {
            return;
        };
        if !parameters.posonlyargs.is_empty()
            || !parameters.kwonlyargs.is_empty()
            || parameters.vararg.is_some()
            || parameters.kwarg.is_some()
        {
            return;
        }
        let mut reads = Reads {
            param: parameter.parameter.name.id.as_str(),
            found: Vec::new(),
        };
        reads.visit_expr(&lambda.body);
        for (name, at) in reads.found.iter().filter(|(n, _)| !columns.contains(n)) {
            self.report_unknown_column(name, columns, display, *at, errors);
        }
    }
}
