//! Column extraction from literal SQL strings, for connector-style loads.
//!
//! Unlike file loads (`usecols=`/`columns=`/`dtype=`), the column set for a database
//! load lives in the query's `SELECT` list. [`columns_from_select`] parses that list
//! conservatively: anything that isn't unambiguously a single named column causes the
//! whole query to be treated as unresolved, on the principle that a false
//! `untracked-dataframe` nudge is recoverable but a false `unknown-column` error (from a
//! short, wrongly-inferred column set) is not.

use sqlparser::ast::{Query, SelectItem, SetExpr, Statement};
use sqlparser::dialect::{
    BigQueryDialect, DatabricksDialect, Dialect, DuckDbDialect, GenericDialect, HiveDialect,
    MySqlDialect, PostgreSqlDialect, RedshiftSqlDialect, SnowflakeDialect, SparkSqlDialect,
};
use sqlparser::parser::Parser;

/// The SQL dialect to parse with, keyed to the connector the query came from.
///
/// Only affects unquoted-identifier case folding (see [`fold_case`]) — the grammar
/// accepted is deliberately the permissive [`GenericDialect`] superset in all cases,
/// since a query written for one engine parsing successfully under another dialect's
/// grammar is not a correctness problem here: we only ever read identifier names out of
/// a successfully parsed `SELECT` list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SqlDialect {
    Generic,
    BigQuery,
    Snowflake,
    Redshift,
    Databricks,
    DuckDb,
    Postgres,
    MySql,
    Hive,
    Spark,
}

impl SqlDialect {
    /// Parse a `[tool.typedframes] sql_dialect` config value. Unknown values fall back
    /// to `Generic` rather than erroring, matching the rest of this crate's policy of
    /// silently defaulting on malformed config.
    pub(crate) fn from_config_str(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "bigquery" => Self::BigQuery,
            "snowflake" => Self::Snowflake,
            "redshift" => Self::Redshift,
            "databricks" => Self::Databricks,
            "duckdb" => Self::DuckDb,
            "postgres" | "postgresql" => Self::Postgres,
            "mysql" => Self::MySql,
            "hive" => Self::Hive,
            "spark" | "sparksql" => Self::Spark,
            _ => Self::Generic,
        }
    }

    fn as_dialect(self) -> Box<dyn Dialect> {
        match self {
            Self::Generic => Box::new(GenericDialect {}),
            Self::BigQuery => Box::new(BigQueryDialect {}),
            Self::Snowflake => Box::new(SnowflakeDialect {}),
            Self::Redshift => Box::new(RedshiftSqlDialect {}),
            Self::Databricks => Box::new(DatabricksDialect {}),
            Self::DuckDb => Box::new(DuckDbDialect {}),
            Self::Postgres => Box::new(PostgreSqlDialect {}),
            Self::MySql => Box::new(MySqlDialect {}),
            Self::Hive => Box::new(HiveDialect {}),
            Self::Spark => Box::new(SparkSqlDialect {}),
        }
    }

    /// Fold an *unquoted* identifier the way this engine resolves it at runtime.
    ///
    /// Quoted identifiers are never folded (callers must check `quote_style` themselves
    /// before calling this) — quoting is exactly the escape hatch users reach for to
    /// preserve case, on every engine in this list.
    fn fold_case(self, ident: &str) -> String {
        match self {
            // Snowflake and Oracle-family engines uppercase unquoted identifiers.
            Self::Snowflake => ident.to_ascii_uppercase(),
            // Postgres/Redshift lowercase unquoted identifiers.
            Self::Postgres | Self::Redshift => ident.to_ascii_lowercase(),
            // BigQuery, DuckDB, Databricks/Spark/Hive, MySQL, and the generic fallback
            // preserve source case for unquoted identifiers.
            Self::Generic
            | Self::BigQuery
            | Self::Databricks
            | Self::DuckDb
            | Self::MySql
            | Self::Hive
            | Self::Spark => ident.to_string(),
        }
    }
}

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
    let dialect_impl = dialect.as_dialect();
    let statements = match Parser::new(dialect_impl.as_ref())
        .try_with_sql(sql)
        .and_then(|mut parser| {
            parser = parser.with_recursion_limit(50);
            parser.parse_statements()
        }) {
        Ok(stmts) => stmts,
        Err(_) => return SqlOutcome::Unparsed,
    };

    let [statement] = statements.as_slice() else {
        return SqlOutcome::Unparsed;
    };

    let Statement::Query(query) = statement else {
        return SqlOutcome::Unparsed;
    };

    columns_from_query(query, dialect)
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
mod tests {
    use super::*;

    fn cols(sql: &str) -> SqlOutcome {
        columns_from_select(sql, SqlDialect::Generic)
    }

    fn cols_d(sql: &str, dialect: SqlDialect) -> SqlOutcome {
        columns_from_select(sql, dialect)
    }

    #[test]
    fn plain_select_list() {
        assert_eq!(
            cols("SELECT order_id, amount FROM orders"),
            SqlOutcome::Columns(vec!["order_id".to_string(), "amount".to_string()])
        );
    }

    #[test]
    fn aliased_columns() {
        assert_eq!(
            cols("SELECT order_id AS id, amount AS total FROM orders"),
            SqlOutcome::Columns(vec!["id".to_string(), "total".to_string()])
        );
    }

    #[test]
    fn compound_identifier_takes_last_part() {
        assert_eq!(
            cols("SELECT o.order_id, o.amount FROM orders o"),
            SqlOutcome::Columns(vec!["order_id".to_string(), "amount".to_string()])
        );
    }

    #[test]
    fn quoted_identifier_preserves_case_verbatim() {
        assert_eq!(
            cols_d("SELECT \"OrderId\" FROM orders", SqlDialect::Postgres),
            SqlOutcome::Columns(vec!["OrderId".to_string()])
        );
    }

    #[test]
    fn snowflake_folds_unquoted_identifiers_uppercase() {
        assert_eq!(
            cols_d("SELECT order_id FROM orders", SqlDialect::Snowflake),
            SqlOutcome::Columns(vec!["ORDER_ID".to_string()])
        );
    }

    #[test]
    fn postgres_folds_unquoted_identifiers_lowercase() {
        assert_eq!(
            cols_d("SELECT \"MixedCase\" FROM orders", SqlDialect::Postgres),
            SqlOutcome::Columns(vec!["MixedCase".to_string()])
        );
    }

    #[test]
    fn bigquery_preserves_unquoted_case() {
        assert_eq!(
            cols_d("SELECT OrderId FROM orders", SqlDialect::BigQuery),
            SqlOutcome::Columns(vec!["OrderId".to_string()])
        );
    }

    #[test]
    fn bare_star_is_wildcard() {
        assert_eq!(cols("SELECT * FROM orders"), SqlOutcome::Wildcard);
    }

    #[test]
    fn qualified_star_is_wildcard() {
        assert_eq!(cols("SELECT o.* FROM orders o"), SqlOutcome::Wildcard);
    }

    #[test]
    fn star_with_except_is_still_wildcard() {
        assert_eq!(
            cols("SELECT * EXCEPT (internal_notes) FROM orders"),
            SqlOutcome::Wildcard
        );
    }

    #[test]
    fn unaliased_function_call_is_unparsed() {
        assert_eq!(
            cols("SELECT order_id, COUNT(*) FROM orders GROUP BY order_id"),
            SqlOutcome::Unparsed
        );
    }

    #[test]
    fn aliased_function_call_resolves() {
        assert_eq!(
            cols("SELECT order_id, COUNT(*) AS n FROM orders GROUP BY order_id"),
            SqlOutcome::Columns(vec!["order_id".to_string(), "n".to_string()])
        );
    }

    #[test]
    fn arithmetic_expression_without_alias_is_unparsed() {
        assert_eq!(
            cols("SELECT order_id, unit_price * quantity FROM orders"),
            SqlOutcome::Unparsed
        );
    }

    #[test]
    fn cte_with_wildcard_body_is_wildcard() {
        assert_eq!(
            cols("WITH recent AS (SELECT * FROM orders) SELECT * FROM recent"),
            SqlOutcome::Wildcard
        );
    }

    #[test]
    fn cte_with_explicit_columns_resolves() {
        assert_eq!(
            cols("WITH recent AS (SELECT order_id FROM orders) SELECT order_id FROM recent"),
            SqlOutcome::Columns(vec!["order_id".to_string()])
        );
    }

    #[test]
    fn union_with_matching_columns_resolves() {
        assert_eq!(
            cols("SELECT order_id FROM orders_a UNION SELECT order_id FROM orders_b"),
            SqlOutcome::Columns(vec!["order_id".to_string()])
        );
    }

    #[test]
    fn union_with_disagreeing_columns_is_unparsed() {
        assert_eq!(
            cols("SELECT order_id FROM orders_a UNION SELECT customer_id FROM customers_b"),
            SqlOutcome::Unparsed
        );
    }

    #[test]
    fn multiple_statements_is_unparsed() {
        assert_eq!(
            cols("SELECT order_id FROM orders; SELECT customer_id FROM customers"),
            SqlOutcome::Unparsed
        );
    }

    #[test]
    fn insert_returning_is_unparsed() {
        assert_eq!(
            cols("INSERT INTO orders (order_id) VALUES (1) RETURNING order_id"),
            SqlOutcome::Unparsed
        );
    }

    #[test]
    fn qmark_placeholder_still_parses() {
        assert_eq!(
            cols("SELECT order_id FROM orders WHERE customer_id = ?"),
            SqlOutcome::Columns(vec!["order_id".to_string()])
        );
    }

    #[test]
    fn garbage_sql_is_unparsed() {
        assert_eq!(cols("not even close to sql"), SqlOutcome::Unparsed);
    }

    #[test]
    fn empty_string_is_unparsed() {
        assert_eq!(cols(""), SqlOutcome::Unparsed);
    }

    #[test]
    fn from_config_str_unknown_value_falls_back_to_generic() {
        assert_eq!(
            SqlDialect::from_config_str("cockroachdb"),
            SqlDialect::Generic
        );
    }

    #[test]
    fn from_config_str_is_case_insensitive() {
        assert_eq!(
            SqlDialect::from_config_str("SNOWFLAKE"),
            SqlDialect::Snowflake
        );
    }
}
