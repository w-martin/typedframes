//! Column extraction from literal SQL strings, for connector-style loads.
//!
//! Unlike file loads (`usecols=`/`columns=`/`dtype=`), the column set for a database
//! load lives in the query's `SELECT` list. [`columns_from_select`] parses that list
//! conservatively: anything that isn't unambiguously a single named column causes the
//! whole query to be treated as unresolved, on the principle that a false
//! `untracked-dataframe` nudge is recoverable but a false `unknown-column` error (from a
//! short, wrongly-inferred column set) is not.

use sqlparser::ast::{Query, SelectItem, SetExpr};
use sqlparser::dialect::Dialect;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::Token;

mod dialect;

pub(crate) use dialect::*;

/// Result of attempting to read a column set out of a literal SQL string.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SqlOutcome {
    /// Every projected item resolved to exactly one named column, in order.
    Columns(Vec<String>),
    /// The query (or a branch of a set operation) projects `*` or `alias.*` — the
    /// column set is real but not statically knowable here.
    Wildcard,
    /// Not parseable as a single, unambiguous `SELECT`: a parse error, more than one
    /// statement, a non-`Query` statement, a projected expression with no derivable
    /// name (function call, arithmetic, `CASE`, cast, subquery), or a set operation
    /// whose branches disagree.
    Unparsed,
}

/// Extract the column set from a literal SQL string's `SELECT` list.
///
/// Deliberately conservative: any ambiguity anywhere in the query causes `Unparsed` (or
/// `Wildcard`) for the *entire* query rather than a partial, silently-short column list.
/// A short inferred column set is worse than no inference at all — it manufactures false
/// `unknown-column` errors on real columns that happen not to have been resolved.
pub(crate) fn columns_from_select(sql: &str, dialect: SqlDialect) -> SqlOutcome {
    // Grammar is always the permissive PermissiveDialect superset, regardless of
    // `dialect` — `dialect` only controls identifier case folding (`fold_case`) once
    // parsing has already succeeded. A query written for one engine failing to parse
    // under another engine's *stricter* grammar (e.g. Redshift's dialect rejecting a
    // `?` placeholder, or plain GenericDialect rejecting T-SQL's `[bracket]`
    // identifiers — both perfectly valid SQL text) would otherwise cost real
    // inferences for no correctness benefit — we only ever read identifier names out
    // of a successfully parsed `SELECT` list, never validate engine-specific syntax.
    match parse_single_query(sql) {
        Some(query) => columns_from_query(&query, dialect),
        None => SqlOutcome::Unparsed,
    }
}

/// The one query `sql` holds, with any `;` around it; `None` for anything else. Parsing a query
/// directly, not a statement list, leaves the statement grammar out of the binary.
fn parse_single_query(sql: &str) -> Option<Box<Query>> {
    let mut parser = Parser::new(&PermissiveDialect)
        .try_with_sql(sql)
        .ok()?
        .with_recursion_limit(50);
    while parser.consume_token(&Token::SemiColon) {}
    let query = parser.parse_query().ok()?;
    while parser.consume_token(&Token::SemiColon) {}
    (parser.peek_token().token == Token::EOF).then_some(query)
}

fn columns_from_query(query: &Query, dialect: SqlDialect) -> SqlOutcome {
    columns_from_set_expr(&query.body, dialect)
}

fn columns_from_set_expr(set_expr: &SetExpr, dialect: SqlDialect) -> SqlOutcome {
    match set_expr {
        SetExpr::Select(select) => columns_from_projection(&select.projection, dialect),
        SetExpr::Query(inner) => columns_from_query(inner, dialect),
        SetExpr::SetOperation { left, right, .. } => {
            let left_cols = columns_from_set_expr(left, dialect);
            let right_cols = columns_from_set_expr(right, dialect);
            if left_cols == right_cols {
                left_cols
            } else {
                // Branches disagree (or either side is itself ambiguous) — stricter
                // than any engine, which always takes names from the first branch.
                SqlOutcome::Unparsed
            }
        }
        // VALUES, INSERT/UPDATE/DELETE/MERGE RETURNING, and bare TABLE references
        // carry no resolvable projection list here.
        _ => SqlOutcome::Unparsed,
    }
}

fn columns_from_projection(projection: &[SelectItem], dialect: SqlDialect) -> SqlOutcome {
    if projection.is_empty() {
        return SqlOutcome::Unparsed;
    }

    let mut columns = Vec::with_capacity(projection.len());
    for item in projection {
        match item {
            SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _) => {
                return SqlOutcome::Wildcard;
            }
            SelectItem::ExprWithAlias { alias, .. } => {
                columns.push(fold_ident(alias, dialect));
            }
            SelectItem::UnnamedExpr(expr) => match unnamed_expr_name(expr, dialect) {
                Some(name) => columns.push(name),
                // Function calls, arithmetic, CASE, casts, subqueries: the
                // driver-assigned name isn't statically knowable. Bail on the whole
                // query rather than silently dropping this one projected item.
                None => return SqlOutcome::Unparsed,
            },
            // `SELECT a, b AS x, y AS z` (Spark-only multi-alias) — not used for
            // ordinary column projections; treat as unresolved rather than guess.
            SelectItem::ExprWithAliases { .. } => return SqlOutcome::Unparsed,
        }
    }

    SqlOutcome::Columns(columns)
}

/// The column name produced by an unaliased projection expression, if any.
///
/// Only a bare identifier (`a`) or compound identifier (`t.a`, taking the last part)
/// has a statically-known name without aliasing; everything else returns `None`.
fn unnamed_expr_name(expr: &sqlparser::ast::Expr, dialect: SqlDialect) -> Option<String> {
    use sqlparser::ast::Expr;
    match expr {
        Expr::Identifier(ident) => Some(fold_ident(ident, dialect)),
        Expr::CompoundIdentifier(parts) => parts.last().map(|i| fold_ident(i, dialect)),
        _ => None,
    }
}

fn fold_ident(ident: &sqlparser::ast::Ident, dialect: SqlDialect) -> String {
    if ident.quote_style.is_some() {
        ident.value.clone()
    } else {
        dialect.fold_case(&ident.value)
    }
}

#[cfg(test)]
mod tests;
