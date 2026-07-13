# Great Expectations vs. typedframes

## Problem Domains

**Great Expectations (GX)** is a data quality framework for runtime validation:
- Assert column existence, types, and distributions
- Profile data and detect anomalies
- Validate data in production pipelines
- Runtime: data assertions on actual values

**typedframes** is a type-safe DataFrame schema system:
- Static type checking for column access in code
- Catch column name typos at lint time (mypy)
- Define DataFrame structure as Python types
- Lint time: prevent access to non-existent columns before code runs

## What Each Tool Does

### Great Expectations Validates
- Column values are of expected type
- Column has no null values
- Column values satisfy statistical constraints
- Data quality metrics and profiling
- Data freshness, schema evolution

### typedframes Validates
- Column names are correct (mypy catches typos)
- Column access is type-safe
- DataFrame structure matches schema definition
- Prevents KeyError from column access mistakes

## What Each Tool Does NOT Do

### Great Expectations Does NOT
- Prevent column access errors in code (runtime KeyError)
- Provide static type checking for DataFrames
- Catch typos in column names at lint time
- Enforce type hints for code using DataFrames

### typedframes Does NOT
- Assert data quality (values, distributions, nullability)
- Profile or analyze data
- Detect schema drift in production
- Validate runtime data constraints

## Example Comparison

### Data Quality Problem (GX Solution)
```python
import pandas as pd
import great_expectations as gx

df = pd.DataFrame({"age": [25, -5, 30]})  # Invalid: negative age

# GX catches this at runtime
context = gx.get_context()
validator = context.get_validator(...)
validator.expect_column_values_to_be_between("age", 0, 120)
# FAILS: age = -5 violates expectation
```

### Column Access Problem (typedframes Solution)
```python
from typing import Annotated
import pandas as pd
from typedframes import BaseSchema, Column

class AgeSchema(BaseSchema):
    age = Column(type=int)

def get_age(df: Annotated[pd.DataFrame, AgeSchema]) -> pd.Series:
    return df["age"]  # OK
    return df["agee"]  # mypy ERROR: typo caught at lint time

# typedframes catches this before code runs
```

## When to Use Each

**Use Great Expectations for:**
- Data pipeline quality assurance
- Detecting bad data before processing
- Monitoring data freshness and drift
- Business rule validation (e.g., "prices > 0")

**Use typedframes for:**
- Type-safe DataFrame access in code
- Preventing column name typos
- Documenting DataFrame structure
- Enabling IDE autocomplete for columns

## Files in This Example

- `a_working_example.py` — GX v1.18.0 runtime validation demo
- `b_static_analysis.py` — Shows GX cannot catch column access errors
- `c_typedframes_comparison.py` — Shows typedframes schema and annotation pattern
- `mypy.ini` — Basic mypy config (no column-level checking)
- `mypy_typedframes.ini` — Config with typedframes.mypy plugin

## Versions

- Great Expectations: 1.18.0 (installed: v1.4.0+ requirement matches)
- typedframes: 0.3.0
- pandas: 3.0.3
- mypy: 2.1.0

## Running the Examples

```bash
# Install
cd examples/great_expectations
source .venv/bin/activate

# Run GX example
python a_working_example.py

# Check GX does not catch column errors
mypy b_static_analysis.py --config-file mypy.ini

# Check typedframes catches column errors
mypy c_typedframes_comparison.py --config-file mypy_typedframes.ini
```
