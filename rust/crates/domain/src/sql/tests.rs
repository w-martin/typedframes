use super::*;

fn cols(sql: &str) -> SqlOutcome {
    columns_from_select(sql, SqlDialect::Generic)
}

fn cols_d(sql: &str, dialect: SqlDialect) -> SqlOutcome {
    columns_from_select(sql, dialect)
}

// The statement-list path `parse_single_query` replaced.
fn via_statements(sql: &str) -> SqlOutcome {
    let parsed = Parser::new(&PermissiveDialect)
        .try_with_sql(sql)
        .and_then(|parser| parser.with_recursion_limit(50).parse_statements());
    match parsed.as_deref() {
        Ok([sqlparser::ast::Statement::Query(query)]) => {
            columns_from_query(query, SqlDialect::Generic)
        }
        _ => SqlOutcome::Unparsed,
    }
}

#[test]
fn single_query_parse_agrees_with_the_statement_list_parse() {
    let inputs = [
        "SELECT a, b FROM t",
        "SELECT a FROM t;",
        ";SELECT a FROM t",
        ";;SELECT a FROM t;;",
        "SELECT a FROM t; SELECT b FROM u",
        "SELECT a FROM t;; SELECT b FROM u;",
        "SELECT a FROM t SELECT b FROM u",
        "",
        ";",
        ";;",
        "(SELECT a FROM t)",
        "((SELECT a FROM t))",
        "(SELECT a FROM t) UNION (SELECT a FROM u)",
        "SELECT a FROM t UNION SELECT b FROM u",
        "WITH c AS (SELECT a FROM t) SELECT a FROM c",
        "WITH c AS (SELECT a FROM t) INSERT INTO u SELECT a FROM c",
        "VALUES (1, 2)",
        "FROM t SELECT a",
        "TABLE t",
        "INSERT INTO t VALUES (1)",
        "UPDATE t SET a = 1",
        "DELETE FROM t",
        "DROP TABLE t",
        "EXPLAIN SELECT a FROM t",
        "CREATE TABLE t AS SELECT a FROM u",
        "SELECT",
        "SELECT a FROM",
        "SELECT a, FROM t",
        "SELECT * FROM t",
        "SELECT a + 1 FROM t",
        "SELECT 'x;y' AS a FROM t",
        "SELECT a FROM t -- trailing; comment",
        "/* lead */ SELECT a FROM t",
        "SELECT a FROM t WHERE b = ?",
        "SELECT [a b] FROM t",
        "SELECT `a` FROM t;\n",
        "\n\t SELECT a FROM t \n",
        "SELECT a FROM t END",
        "not sql at all",
    ];
    for sql in inputs {
        if sql == "SELECT a FROM t END" {
            // `parse_statements` stops at a stray `END`; a query followed by anything is `Unparsed` here.
            assert_eq!(SqlOutcome::Unparsed, cols(sql), "{sql:?}");
        } else {
            assert_eq!(via_statements(sql), cols(sql), "{sql:?}");
        }
    }
}

#[test]
fn deeply_nested_input_is_unparsed_not_a_stack_overflow() {
    let sql = format!("SELECT {}1{} AS a", "(".repeat(5000), ")".repeat(5000));
    let outcome = std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(move || cols(&sql))
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(SqlOutcome::Unparsed, outcome);
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
fn qmark_placeholder_still_parses_under_a_non_generic_dialect() {
    // Regression test: grammar must always be PermissiveDialect regardless of
    // which SqlDialect is passed — dialect only controls case folding. This
    // previously used the specific engine's (stricter) grammar for parsing too, so
    // a query that's perfectly valid SQL text failed to parse under e.g.
    // Redshift's dialect even though it parses fine under the shared grammar.
    assert_eq!(
        cols_d(
            "SELECT order_id FROM orders WHERE customer_id = ?",
            SqlDialect::Redshift
        ),
        SqlOutcome::Columns(vec!["order_id".to_string()])
    );
}

#[test]
fn bracket_quoted_identifiers_parse() {
    // Regression test: T-SQL's [bracket] quoting (SQL Server / Azure SQL /
    // Synapse / Fabric Warehouse) — sqlparser's plain GenericDialect only accepts
    // `"` and `` ` `` as quote characters, so this failed to parse (Unparsed)
    // before PermissiveDialect added `[` support.
    assert_eq!(
        cols("SELECT [OrderID], [CustomerID] FROM [dbo].[Orders]"),
        SqlOutcome::Columns(vec!["OrderID".to_string(), "CustomerID".to_string()])
    );
}

#[test]
fn bracket_quoted_identifier_preserves_case_verbatim() {
    // Quoting (bracket or double-quote) is the escape hatch that preserves case on
    // every engine in this list — a case-folding dialect must not touch it.
    assert_eq!(
        cols_d("SELECT [OrderId] FROM orders", SqlDialect::Postgres),
        SqlOutcome::Columns(vec!["OrderId".to_string()])
    );
}

#[test]
fn mssql_dialect_does_not_fold_unquoted_identifiers() {
    // SQL Server preserves the case an unquoted identifier was declared with
    // rather than rewriting it — same "preserve" bucket as Generic/BigQuery/etc.
    assert_eq!(
        cols_d("SELECT OrderId FROM orders", SqlDialect::MsSql),
        SqlOutcome::Columns(vec!["OrderId".to_string()])
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

#[test]
fn from_config_str_recognizes_azure_sql_server_family_aliases() {
    for alias in ["mssql", "sqlserver", "synapse", "fabric", "SYNAPSE"] {
        assert_eq!(
            SqlDialect::from_config_str(alias),
            SqlDialect::MsSql,
            "alias: {alias}"
        );
    }
}
