# CLI

The `typedframes` command-line interface runs the Rust-based static checker on Python source files.

## Basic usage

```shell
# Check a single file
typedframes check pipeline.py

# Check an entire directory (builds a cross-file project index)
typedframes check src/

# Check without building the project index (each file checked independently)
typedframes check src/ --no-index

# Enable untracked-dataframe warnings for bare DataFrame loads (off by default)
typedframes check src/ --strict-ingest

# Output formats
typedframes check src/ --output-format text    # default — ty-style, auto-colored in terminal
typedframes check src/ --output-format json    # machine-readable JSON
typedframes check src/ --output-format github  # GitHub Actions annotations
```

## Supported file formats

The checker reads column information from load calls for all common formats:

```python
# pandas
pd.read_csv("data.csv", usecols=["a", "b"])
pd.read_parquet("data.parquet", columns=["a", "b"])
pd.read_json("data.json", dtype={"a": int, "b": str})
pd.read_excel("data.xlsx", usecols=["a", "b"])

# polars
pl.read_csv("data.csv", columns=["a", "b"])
pl.read_parquet("data.parquet", columns=["a", "b"])
pl.read_json("data.json", schema={"a": int, "b": str})
```

Any `usecols=` (pandas) or `columns=` / `schema=` (polars) argument teaches the checker
which columns are available, regardless of file format.

## Output format

```
src/pipeline.py:42:8: error[unknown-column] Column 'revenue' not in OrderSchema
src/pipeline.py:57:8: error[reserved-name] Column 'user_id' renamed to 'customer_id', use 'customer_id'
src/pipeline.py:10:1: warning[untracked-dataframe] columns unknown at lint time; specify usecols= or annotate
src/pipeline.py:12:5: error[missing-column] 'customers' passed to contact_label (transforms.py:2) is missing
  column(s) {email} — available: {customer_id, name, region}, required: {email, name}
```

The format matches ty and ruff: `file:line:col: severity[code] message`. Most editors,
CI systems, and LSP clients parse this automatically. Colors are applied when the output
is a terminal (TTY); piping or redirecting strips them.

## Error codes

| Code | Meaning | Default |
|------|---------|---------|
| `unknown-column` | Column not found in schema or inferred set | Always shown |
| `reserved-name` | Column was renamed — use the new name | Always shown |
| `untracked-dataframe` | Bare DataFrame load — no column info for checker | Off (use `--strict-ingest`) |
| `dropped-unknown-column` | Dropped column doesn't exist in schema | Off (use `--strict-ingest`) |
| `missing-column` | Argument's columns don't satisfy the called function's parameter contract | Always shown |

## Project-level configuration

Add to `pyproject.toml` to disable all warnings project-wide:

```toml
[tool.typedframes]
enabled  = true
warnings = false
```

---

::: typedframes.cli.main
