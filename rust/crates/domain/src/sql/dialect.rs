use super::*;

/// The grammar every query is parsed with, regardless of `SqlDialect` — see
/// [`columns_from_select`]. Identical to [`sqlparser::dialect::GenericDialect`] (its
/// permissive superset of accepted syntax) with one addition: `[bracket-quoted]`
/// identifiers, T-SQL's convention (SQL Server, Azure SQL, Synapse, Fabric Warehouse),
/// which `GenericDialect` doesn't accept — verified directly:
/// `columns_from_select("SELECT [OrderID] FROM [dbo].[Orders]", ...)` returns
/// `Unparsed` under plain `GenericDialect`, since it only treats `"` and `` ` `` as
/// quote characters.
///
/// `sqlparser` has no dialect-composition mechanism (a `Dialect` impl can't "inherit"
/// another's overrides), so this duplicates `GenericDialect`'s method bodies verbatim
/// rather than picking a stricter single-engine dialect — keep it in sync with
/// `GenericDialect` if upgrading the `sqlparser` dependency changes those defaults.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct PermissiveDialect;

impl Dialect for PermissiveDialect {
    fn is_delimited_identifier_start(&self, ch: char) -> bool {
        ch == '"' || ch == '`' || ch == '['
    }

    fn is_identifier_start(&self, ch: char) -> bool {
        ch.is_alphabetic() || ch == '_' || ch == '#' || ch == '@'
    }

    fn is_identifier_part(&self, ch: char) -> bool {
        ch.is_alphabetic()
            || ch.is_ascii_digit()
            || ch == '@'
            || ch == '$'
            || ch == '#'
            || ch == '_'
    }

    fn supports_unicode_string_literal(&self) -> bool {
        true
    }

    fn supports_partition_by_after_order_by(&self) -> bool {
        true
    }

    fn supports_array_join_syntax(&self) -> bool {
        true
    }

    fn supports_group_by_expr(&self) -> bool {
        true
    }

    fn supports_group_by_with_modifier(&self) -> bool {
        true
    }

    fn supports_left_associative_joins_without_parens(&self) -> bool {
        true
    }

    fn supports_connect_by(&self) -> bool {
        true
    }

    fn supports_match_recognize(&self) -> bool {
        true
    }

    fn supports_pipe_operator(&self) -> bool {
        true
    }

    fn supports_start_transaction_modifier(&self) -> bool {
        true
    }

    fn supports_window_function_null_treatment_arg(&self) -> bool {
        true
    }

    fn supports_dictionary_syntax(&self) -> bool {
        true
    }

    fn supports_window_clause_named_window_reference(&self) -> bool {
        true
    }

    fn supports_parenthesized_set_variables(&self) -> bool {
        true
    }

    fn supports_select_wildcard_except(&self) -> bool {
        true
    }

    fn support_map_literal_syntax(&self) -> bool {
        true
    }

    fn allow_extract_custom(&self) -> bool {
        true
    }

    fn allow_extract_single_quotes(&self) -> bool {
        true
    }

    fn supports_extract_comma_syntax(&self) -> bool {
        true
    }

    fn supports_create_view_comment_syntax(&self) -> bool {
        true
    }

    fn supports_parens_around_table_factor(&self) -> bool {
        true
    }

    fn supports_values_as_table_factor(&self) -> bool {
        true
    }

    fn supports_create_index_with_clause(&self) -> bool {
        true
    }

    fn supports_explain_with_utility_options(&self) -> bool {
        true
    }

    fn supports_limit_comma(&self) -> bool {
        true
    }

    fn supports_update_order_by(&self) -> bool {
        true
    }

    fn supports_from_first_select(&self) -> bool {
        true
    }

    fn supports_projection_trailing_commas(&self) -> bool {
        true
    }

    fn supports_asc_desc_in_column_definition(&self) -> bool {
        true
    }

    fn supports_try_convert(&self) -> bool {
        true
    }

    fn supports_bitwise_shift_operators(&self) -> bool {
        true
    }

    fn supports_comment_on(&self) -> bool {
        true
    }

    fn supports_load_extension(&self) -> bool {
        true
    }

    fn supports_named_fn_args_with_assignment_operator(&self) -> bool {
        true
    }

    fn supports_struct_literal(&self) -> bool {
        true
    }

    fn supports_empty_projections(&self) -> bool {
        true
    }

    fn supports_nested_comments(&self) -> bool {
        true
    }

    fn supports_multiline_comment_hints(&self) -> bool {
        true
    }

    fn supports_user_host_grantee(&self) -> bool {
        true
    }

    fn supports_string_escape_constant(&self) -> bool {
        true
    }

    fn supports_array_typedef_with_brackets(&self) -> bool {
        true
    }

    fn supports_match_against(&self) -> bool {
        true
    }

    fn supports_set_names(&self) -> bool {
        true
    }

    fn supports_comma_separated_set_assignments(&self) -> bool {
        true
    }

    fn supports_filter_during_aggregation(&self) -> bool {
        true
    }

    fn supports_select_wildcard_exclude(&self) -> bool {
        true
    }

    fn supports_data_type_signed_suffix(&self) -> bool {
        true
    }

    fn supports_interval_options(&self) -> bool {
        true
    }

    fn supports_quote_delimited_string(&self) -> bool {
        true
    }

    fn supports_select_wildcard_replace(&self) -> bool {
        true
    }

    fn supports_select_wildcard_ilike(&self) -> bool {
        true
    }

    fn supports_select_wildcard_rename(&self) -> bool {
        true
    }

    fn supports_optimize_table(&self) -> bool {
        true
    }

    fn supports_install(&self) -> bool {
        true
    }

    fn supports_detach(&self) -> bool {
        true
    }

    fn supports_prewhere(&self) -> bool {
        true
    }

    fn supports_with_fill(&self) -> bool {
        true
    }

    fn supports_limit_by(&self) -> bool {
        true
    }

    fn supports_interpolate(&self) -> bool {
        true
    }

    fn supports_settings(&self) -> bool {
        true
    }

    fn supports_select_format(&self) -> bool {
        true
    }

    fn supports_comment_optimizer_hint(&self) -> bool {
        true
    }

    fn supports_constraint_keyword_without_name(&self) -> bool {
        true
    }

    fn supports_key_column_option(&self) -> bool {
        true
    }

    fn supports_comma_separated_trim(&self) -> bool {
        true
    }

    fn supports_cte_without_as(&self) -> bool {
        true
    }

    fn supports_select_item_multi_column_alias(&self) -> bool {
        true
    }

    fn supports_xml_expressions(&self) -> bool {
        true
    }
}

/// The SQL dialect to parse with, keyed to the connector the query came from.
///
/// Only affects unquoted-identifier case folding (see [`fold_case`]) — the grammar
/// accepted is deliberately the permissive [`PermissiveDialect`] superset in all cases,
/// since a query written for one engine parsing successfully under another dialect's
/// grammar is not a correctness problem here: we only ever read identifier names out of
/// a successfully parsed `SELECT` list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SqlDialect {
    #[default]
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
    // SQL Server / Azure SQL Database / Azure Synapse (dedicated and serverless SQL
    // pools) / Microsoft Fabric Warehouse. Behaves identically to `Generic` in
    // `fold_case` — SQL Server preserves the case an unquoted identifier was declared
    // with rather than rewriting it, so there's nothing to fold — but is its own named
    // variant (rather than relying on `from_config_str`'s Generic fallback) so
    // `sql_dialect = "synapse"` is a deliberate, documented choice instead of a
    // silent coincidence.
    MsSql,
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
            "mssql" | "sqlserver" | "synapse" | "fabric" => Self::MsSql,
            _ => Self::Generic,
        }
    }

    /// Fold an *unquoted* identifier the way this engine resolves it at runtime.
    ///
    /// Quoted identifiers are never folded (callers must check `quote_style` themselves
    /// before calling this) — quoting is exactly the escape hatch users reach for to
    /// preserve case, on every engine in this list.
    pub(super) fn fold_case(self, ident: &str) -> String {
        match self {
            // Snowflake and Oracle-family engines uppercase unquoted identifiers.
            Self::Snowflake => ident.to_ascii_uppercase(),
            // Postgres/Redshift lowercase unquoted identifiers.
            Self::Postgres | Self::Redshift => ident.to_ascii_lowercase(),
            // BigQuery, DuckDB, Databricks/Spark/Hive, MySQL, SQL Server/Synapse/
            // Fabric, and the generic fallback preserve source case for unquoted
            // identifiers.
            Self::Generic
            | Self::BigQuery
            | Self::Databricks
            | Self::DuckDb
            | Self::MySql
            | Self::Hive
            | Self::Spark
            | Self::MsSql => ident.to_string(),
        }
    }
}
