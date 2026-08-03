# API Reference

Annotate DataFrames with `Annotated[pd.DataFrame, MySchema]` or
`Annotated[pl.DataFrame, MySchema]` and the static checker validates column access
at lint time.

| Module | Contents |
|--------|----------|
| [Core](core.md) | `BaseSchema`, `Column`, `ColumnSet`, `ColumnGroup` — schema definition |
| [Schema Algebra](schema_algebra.md) | `combine_schemas`, `SchemaConflictError` — composing schemas |
| [Pandera Integration](pandera.md) | `to_pandera_schema` — runtime validation bridge |
| [Exceptions](exceptions.md) | `ColumnGroupError`, `MissingDependencyError` |
| [pandas](pandas.md) | `PandasFrame` — deprecated runtime enhancement |
| [polars](polars.md) | `PolarsFrame` — deprecated alias |
| [CLI](cli.md) | `typedframes check` command |
