"""Integration tests for typedframes checker."""

import json
import tempfile
import unittest
from pathlib import Path

from typedframes._rust_checker import build_project_index, check_file  # ty: ignore[unresolved-import]


class TestTypedFramesCheckerIntegration(unittest.TestCase):
    """Integration tests for the Rust checker."""

    def test_should_detect_missing_column(self) -> None:
        """Test that the checker detects missing columns."""
        # arrange
        example_file = str(Path("examples/typedframes_example.py").absolute())

        # act
        result = check_file(example_file)

        # assert
        self.assertIn("Column 'wrong_column' does not exist", result)

    def test_should_suggest_typo_correction(self) -> None:
        """Test that the checker suggests corrections for typos."""
        # arrange
        example_file = str(Path("examples/typedframes_example.py").absolute())

        # act
        result = check_file(example_file)

        # assert
        self.assertIn("did you mean 'user_id'?", result)

    def test_should_catch_polars_column_errors(self) -> None:
        """Test that the checker catches column errors in polars examples."""
        # arrange
        example_file = str(Path("examples/typedframes_example.py").absolute())

        # act
        result = check_file(example_file)

        # assert
        self.assertIn("typo_column", result)

    def test_should_run_via_python_extension(self) -> None:
        """Test that the Rust checker works via Python extension."""
        # arrange
        example_file = str(Path("examples/typedframes_example.py").absolute())

        # act
        result = check_file(example_file)

        # assert
        self.assertIn("wrong_column", result)
        self.assertIn("does not exist", result)

    def test_should_warn_about_reserved_method_names(self) -> None:
        """Test that the checker warns about column names that shadow pandas/polars methods."""
        # arrange
        source = """
from typedframes import BaseSchema, Column

class BadSchema(BaseSchema):
    mean = Column(type=float)
    user_id = Column(type=int)
"""
        with tempfile.NamedTemporaryFile(mode="w", suffix=".py", delete=False) as f:
            f.write(source)
            temp_file = f.name

        # act
        result = check_file(temp_file)
        errors = json.loads(result)

        # assert
        self.assertEqual(len(errors), 1)
        self.assertIn("mean", errors[0]["message"])
        self.assertIn("conflicts with a pandas/polars method", errors[0]["message"])

        # cleanup
        Path(temp_file).unlink()

    def test_should_build_project_index_with_schema_entries(self) -> None:
        """Test that build_project_index returns MessagePack bytes and the index is usable."""
        # arrange
        schema_source = """
from typedframes import BaseSchema, Column

class ProductSchema(BaseSchema):
    product_id = Column(type=int)
    name = Column(type=str)
    price = Column(type=float)
"""
        with tempfile.TemporaryDirectory() as tmpdir:
            schema_path = Path(tmpdir) / "schema.py"
            schema_path.write_text(schema_source)

            # act
            index_bytes = build_project_index(tmpdir)

            # assert — bytes sanity check
            self.assertIsInstance(index_bytes, bytes)
            self.assertGreater(len(index_bytes), 0)
            # functional check: schema file itself has no column-access errors
            result = check_file(str(schema_path), index_bytes)
            self.assertEqual(json.loads(result), [])

    def test_should_catch_cross_file_column_error(self) -> None:
        """Test that the checker catches column errors when the schema is defined in another file."""
        # arrange
        loaders_source = """
from typedframes import BaseSchema, Column
from typedframes.pandas import PandasFrame

class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)

def load_users() -> PandasFrame[UserSchema]:
    pass
"""
        pipeline_source = """
from loaders import load_users

df = load_users()
print(df["user_id"])
print(df["wrong_column"])
"""
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            (root / "loaders.py").write_text(loaders_source)
            (root / "pipeline.py").write_text(pipeline_source)
            (root / "pyproject.toml").write_text("[tool.typedframes]\nenabled = true\n")

            # act
            index_bytes = build_project_index(tmpdir)
            result = check_file(str(root / "pipeline.py"), index_bytes)
            errors = json.loads(result)

            # assert
            messages = [e["message"] for e in errors]
            self.assertTrue(any("wrong_column" in m for m in messages))
            self.assertFalse(any("user_id" in m for m in messages))

    def test_should_infer_columns_from_multi_column_subscript(self) -> None:
        """Test that a = df[["foo", "bar"]] creates an inferred schema and enforces it."""
        # arrange
        source = """
from typedframes import BaseSchema, Column
from typedframes.pandas import PandasFrame

class MySchema(BaseSchema):
    foo = Column(type=str)
    bar = Column(type=str)
    baz = Column(type=str)

df: PandasFrame[MySchema] = PandasFrame.from_schema(load(), MySchema)
a = df[["foo", "bar"]]
_ = a["baz"]
_ = a["foo"]
"""
        with tempfile.NamedTemporaryFile(mode="w", suffix=".py", delete=False) as f:
            f.write(source)
            temp_file = f.name

        try:
            # act
            result = check_file(temp_file, None)
            errors = json.loads(result)

            # assert — "baz" not in slice, "foo" is fine
            messages = [e["message"] for e in errors]
            self.assertTrue(any("baz" in m for m in messages))
            self.assertFalse(any("'foo'" in m for m in messages))
        finally:
            Path(temp_file).unlink()

    def test_should_catch_error_when_slicing_column_not_in_base(self) -> None:
        """Test that slicing with a column not in the base schema emits an error."""
        # arrange
        source = """
from typedframes import BaseSchema, Column
from typedframes.pandas import PandasFrame

class MySchema(BaseSchema):
    foo = Column(type=str)
    bar = Column(type=str)

df: PandasFrame[MySchema] = PandasFrame.from_schema(load(), MySchema)
a = df[["foo", "missing"]]
"""
        with tempfile.NamedTemporaryFile(mode="w", suffix=".py", delete=False) as f:
            f.write(source)
            temp_file = f.name

        try:
            # act
            result = check_file(temp_file, None)
            errors = json.loads(result)

            # assert
            messages = [e["message"] for e in errors]
            self.assertTrue(any("missing" in m for m in messages))
        finally:
            Path(temp_file).unlink()

    def test_should_propagate_schema_through_row_filter(self) -> None:
        """Test that filter() propagates the base schema."""
        # arrange
        source = """
from typedframes import BaseSchema, Column
from typedframes.pandas import PandasFrame

class MySchema(BaseSchema):
    col = Column(type=str)

df: PandasFrame[MySchema] = PandasFrame.from_schema(load(), MySchema)
filtered = df.filter(df["col"] == "x")
_ = filtered["nonexistent"]
"""
        with tempfile.NamedTemporaryFile(mode="w", suffix=".py", delete=False) as f:
            f.write(source)
            temp_file = f.name

        try:
            # act
            result = check_file(temp_file, None)
            errors = json.loads(result)

            # assert
            messages = [e["message"] for e in errors]
            self.assertTrue(any("nonexistent" in m for m in messages))
        finally:
            Path(temp_file).unlink()

    def test_should_infer_columns_from_polars_select(self) -> None:
        """Test that small = df.select(["foo"]) creates an inferred schema."""
        # arrange
        source = """
from typedframes import BaseSchema, Column
from typedframes.polars import PolarsFrame

class MySchema(BaseSchema):
    foo = Column(type=str)
    bar = Column(type=str)

df: PolarsFrame[MySchema] = PolarsFrame.from_schema(load(), MySchema)
small = df.select(["foo"])
_ = small["foo"]
_ = small["bar"]
"""
        with tempfile.NamedTemporaryFile(mode="w", suffix=".py", delete=False) as f:
            f.write(source)
            temp_file = f.name

        try:
            # act
            result = check_file(temp_file, None)
            errors = json.loads(result)

            # assert — "bar" not in select, "foo" is fine
            messages = [e["message"] for e in errors]
            self.assertTrue(any("'bar'" in m for m in messages))
            self.assertFalse(any("'foo'" in m for m in messages))
        finally:
            Path(temp_file).unlink()

    def test_should_infer_columns_from_drop(self) -> None:
        """Test that trimmed = df.drop(columns=["baz"]) excludes the dropped column."""
        # arrange
        source = """
from typedframes import BaseSchema, Column
from typedframes.pandas import PandasFrame

class MySchema(BaseSchema):
    foo = Column(type=str)
    bar = Column(type=str)
    baz = Column(type=str)

df: PandasFrame[MySchema] = PandasFrame.from_schema(load(), MySchema)
trimmed = df.drop(columns=["baz"])
_ = trimmed["baz"]
_ = trimmed["foo"]
"""
        with tempfile.NamedTemporaryFile(mode="w", suffix=".py", delete=False) as f:
            f.write(source)
            temp_file = f.name

        try:
            # act
            result = check_file(temp_file, None)
            errors = json.loads(result)

            # assert — "baz" dropped, "foo" ok
            messages = [e["message"] for e in errors]
            self.assertTrue(any("'baz'" in m for m in messages))
            self.assertFalse(any("'foo'" in m for m in messages))
        finally:
            Path(temp_file).unlink()

    def test_should_warn_when_dropping_unknown_column(self) -> None:
        """Test that dropping a column not in the schema emits a dropped-unknown-column warning."""
        # arrange
        source = """
from typedframes import BaseSchema, Column
from typedframes.pandas import PandasFrame

class MySchema(BaseSchema):
    foo = Column(type=str)
    bar = Column(type=str)

df: PandasFrame[MySchema] = PandasFrame.from_schema(load(), MySchema)
trimmed = df.drop(columns=["nonexistent"])
"""
        with tempfile.NamedTemporaryFile(mode="w", suffix=".py", delete=False) as f:
            f.write(source)
            temp_file = f.name

        try:
            # act
            result = check_file(temp_file, None)
            errors = json.loads(result)

            # assert
            warnings = [e for e in errors if e.get("severity") == "warning"]
            self.assertTrue(any("nonexistent" in e["message"] for e in warnings))
        finally:
            Path(temp_file).unlink()

    def test_should_infer_columns_from_rename(self) -> None:
        """Test that renamed = df.rename(columns={"foo": "bar"}) updates the column set."""
        # arrange
        source = """
from typedframes import BaseSchema, Column
from typedframes.pandas import PandasFrame

class MySchema(BaseSchema):
    foo = Column(type=str)
    other = Column(type=str)

df: PandasFrame[MySchema] = PandasFrame.from_schema(load(), MySchema)
renamed = df.rename(columns={"foo": "bar"})
_ = renamed["foo"]
_ = renamed["bar"]
"""
        with tempfile.NamedTemporaryFile(mode="w", suffix=".py", delete=False) as f:
            f.write(source)
            temp_file = f.name

        try:
            # act
            result = check_file(temp_file, None)
            errors = json.loads(result)

            # assert — "foo" renamed to "bar", so "foo" should error
            messages = [e["message"] for e in errors]
            self.assertTrue(any("'foo'" in m for m in messages))
            self.assertFalse(any("'bar'" in m for m in messages))
        finally:
            Path(temp_file).unlink()

    def test_should_infer_columns_from_assign(self) -> None:
        """Test that augmented = df.assign(new_col=1) adds the new column."""
        # arrange
        source = """
from typedframes import BaseSchema, Column
from typedframes.pandas import PandasFrame

class MySchema(BaseSchema):
    old = Column(type=str)

df: PandasFrame[MySchema] = PandasFrame.from_schema(load(), MySchema)
augmented = df.assign(new_col=1)
_ = augmented["new_col"]
_ = augmented["old"]
_ = augmented["missing"]
"""
        with tempfile.NamedTemporaryFile(mode="w", suffix=".py", delete=False) as f:
            f.write(source)
            temp_file = f.name

        try:
            # act
            result = check_file(temp_file, None)
            errors = json.loads(result)

            # assert — new_col and old are fine; missing errors
            messages = [e["message"] for e in errors]
            self.assertFalse(any("'new_col'" in m for m in messages))
            self.assertFalse(any("'old'" in m for m in messages))
            self.assertTrue(any("'missing'" in m for m in messages))
        finally:
            Path(temp_file).unlink()

    def test_should_infer_columns_from_load_with_usecols(self) -> None:
        """Test that pd.read_csv with usecols creates an inferred schema."""
        # arrange
        source = """
import pandas as pd

df = pd.read_csv("x.csv", usecols=["a", "b"])
_ = df["a"]
_ = df["c"]
"""
        with tempfile.NamedTemporaryFile(mode="w", suffix=".py", delete=False) as f:
            f.write(source)
            temp_file = f.name

        try:
            # act
            result = check_file(temp_file, None)
            errors = json.loads(result)

            # assert — "c" not in usecols, "a" is fine
            messages = [e["message"] for e in errors]
            self.assertTrue(any("Column 'c' does not exist" in m for m in messages))
            self.assertFalse(any("Column 'a' does not exist" in m for m in messages))
        finally:
            Path(temp_file).unlink()

    def test_should_warn_when_load_has_no_columns_specified(self) -> None:
        """Test that pd.read_csv without column info emits an untracked-dataframe warning."""
        # arrange
        source = """
import pandas as pd

df = pd.read_csv("x.csv")
"""
        with tempfile.NamedTemporaryFile(mode="w", suffix=".py", delete=False) as f:
            f.write(source)
            temp_file = f.name

        try:
            # act
            result = check_file(temp_file, None)
            errors = json.loads(result)

            # assert
            self.assertEqual(len(errors), 1)
            self.assertEqual(errors[0]["severity"], "warning")
            self.assertIn("columns unknown at lint time", errors[0]["message"])
        finally:
            Path(temp_file).unlink()

    def test_should_not_warn_when_load_has_schema_annotation(self) -> None:
        """Test that df: PandasFrame[MySchema] = pd.read_csv(...) does not emit untracked-dataframe."""
        # arrange
        source = """
import pandas as pd
from typedframes import BaseSchema, Column
from typedframes.pandas import PandasFrame

class MySchema(BaseSchema):
    a = Column(type=str)
    b = Column(type=int)

df: PandasFrame[MySchema] = pd.read_csv("x.csv")
_ = df["a"]
_ = df["c"]
"""
        with tempfile.NamedTemporaryFile(mode="w", suffix=".py", delete=False) as f:
            f.write(source)
            temp_file = f.name

        try:
            # act
            result = check_file(temp_file, None)
            errors = json.loads(result)

            # assert — no untracked-dataframe; "c" errors from schema; no warning for "a"
            warnings = [e for e in errors if e.get("severity") == "warning"]
            self.assertEqual(len(warnings), 0)
            messages = [e["message"] for e in errors]
            self.assertTrue(any("'c'" in m for m in messages))
        finally:
            Path(temp_file).unlink()

    def test_should_propagate_inferred_through_filter_chain(self) -> None:
        """Test that an inferred schema propagates through a filter chain."""
        # arrange
        source = """
from typedframes import BaseSchema, Column
from typedframes.pandas import PandasFrame

class MySchema(BaseSchema):
    foo = Column(type=str)
    bar = Column(type=str)
    baz = Column(type=str)

df: PandasFrame[MySchema] = PandasFrame.from_schema(load(), MySchema)
a = df[["foo", "bar"]]
b = a.filter(a["foo"] == "x")
_ = b["foo"]
_ = b["baz"]
"""
        with tempfile.NamedTemporaryFile(mode="w", suffix=".py", delete=False) as f:
            f.write(source)
            temp_file = f.name

        try:
            # act
            result = check_file(temp_file, None)
            errors = json.loads(result)

            # assert — "foo" ok through chain; "baz" not in slice so errors
            messages = [e["message"] for e in errors]
            self.assertTrue(any("'baz'" in m for m in messages))
            # "foo" should not error (it's in the inferred set)
            b_baz_errors = [m for m in messages if "'baz'" in m]
            self.assertGreater(len(b_baz_errors), 0)
        finally:
            Path(temp_file).unlink()

    def test_should_detect_missing_column_at_function_call_site(self) -> None:
        """Test that calling a function with a DataFrame missing a column it needs is caught."""
        # arrange
        loaders_source = """
import pandas as pd

def load(path: str) -> pd.DataFrame:
    df = pd.read_csv(path, usecols=["a", "b"])
    return df
"""
        steps_source = """
import pandas as pd

def postproc(df: pd.DataFrame) -> pd.DataFrame:
    y = df["c"]
    return df
"""
        pipeline_source = """
from loaders import load
from steps import postproc

def process(path: str) -> None:
    df = load(path)
    result = postproc(df)
    print(result)
"""
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            (root / "loaders.py").write_text(loaders_source)
            (root / "steps.py").write_text(steps_source)
            (root / "pipeline.py").write_text(pipeline_source)
            (root / "pyproject.toml").write_text("[tool.typedframes]\nenabled = true\n")

            # act
            index_bytes = build_project_index(tmpdir)
            result = check_file(str(root / "pipeline.py"), index_bytes)
            errors = json.loads(result)

            # assert
            self.assertEqual(len(errors), 1)
            self.assertEqual(errors[0]["code"], "missing-column")
            self.assertIn("missing column(s) {c}", errors[0]["message"])
            self.assertIn("available: {a, b}", errors[0]["message"])

    def test_should_resolve_missing_column_transitively_through_delegate_chain(self) -> None:
        """Test that a missing column surfaces transitively through a chain of delegate calls."""
        # arrange
        loaders_source = """
import pandas as pd

def load(path: str) -> pd.DataFrame:
    df = pd.read_csv(path, usecols=["a", "b"])
    return df
"""
        steps_source = """
import pandas as pd

def preproc(df: pd.DataFrame) -> pd.DataFrame:
    z = df["a"]
    return df

def infer(df: pd.DataFrame) -> pd.DataFrame:
    x = df["b"]
    return df

def postproc(df: pd.DataFrame) -> pd.DataFrame:
    y = df["c"]
    return df

def transform(df: pd.DataFrame) -> pd.DataFrame:
    step1 = preproc(df)
    step2 = infer(step1)
    step3 = postproc(step2)
    return step3
"""
        pipeline_source = """
from loaders import load
from steps import transform

def process(path: str) -> None:
    df = load(path)
    result = transform(df)
    print(result)
"""
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            (root / "loaders.py").write_text(loaders_source)
            (root / "steps.py").write_text(steps_source)
            (root / "pipeline.py").write_text(pipeline_source)
            (root / "pyproject.toml").write_text("[tool.typedframes]\nenabled = true\n")

            # act
            index_bytes = build_project_index(tmpdir)
            result = check_file(str(root / "pipeline.py"), index_bytes)
            errors = json.loads(result)

            # assert — 'c' is only touched two calls deep, inside postproc
            self.assertEqual(len(errors), 1)
            self.assertEqual(errors[0]["code"], "missing-column")
            self.assertIn("missing column(s) {c}", errors[0]["message"])
            self.assertIn("required: {a, b, c}", errors[0]["message"])

    def test_should_validate_schema_annotated_parameter_in_body(self) -> None:
        """Test that a bad access on a schema-annotated parameter is caught inside the function body."""
        # arrange
        source = """
from typedframes import BaseSchema, Column
from typedframes.pandas import PandasFrame

class CustomerSchema(BaseSchema):
    customer_id = Column(type=int)
    name = Column(type=str)

def contact_label(customers: PandasFrame[CustomerSchema]):
    print(customers["name"])
    print(customers["email"])
"""
        with tempfile.NamedTemporaryFile(mode="w", suffix=".py", delete=False) as f:
            f.write(source)
            temp_file = f.name

        try:
            # act
            result = check_file(temp_file, None)
            errors = json.loads(result)

            # assert
            self.assertEqual(len(errors), 1)
            self.assertEqual(errors[0]["code"], "unknown-column")
            self.assertIn("email", errors[0]["message"])
            self.assertIn("CustomerSchema", errors[0]["message"])
        finally:
            Path(temp_file).unlink()

    def test_should_use_schema_annotation_as_authoritative_call_site_contract(self) -> None:
        """Test that a schema annotation's full column list becomes the call-site contract."""
        # arrange
        loaders_source = """
import pandas as pd

def load(path: str) -> pd.DataFrame:
    df = pd.read_csv(path, usecols=["customer_id", "name"])
    return df
"""
        transforms_source = """
from typedframes import BaseSchema, Column
from typedframes.pandas import PandasFrame

class CustomerSchema(BaseSchema):
    customer_id = Column(type=int)
    name = Column(type=str)
    email = Column(type=str)

def contact_label(customers: PandasFrame[CustomerSchema]):
    print(customers["name"])
    return customers
"""
        pipeline_source = """
from loaders import load
from transforms import contact_label

def process(path: str) -> None:
    customers = load(path)
    contact_label(customers)
"""
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            (root / "loaders.py").write_text(loaders_source)
            (root / "transforms.py").write_text(transforms_source)
            (root / "pipeline.py").write_text(pipeline_source)
            (root / "pyproject.toml").write_text("[tool.typedframes]\nenabled = true\n")

            # act
            index_bytes = build_project_index(tmpdir)
            result = check_file(str(root / "pipeline.py"), index_bytes)
            errors = json.loads(result)

            # assert — "email" is declared on CustomerSchema but never subscripted in
            # contact_label's body; it must still be part of the contract
            self.assertEqual(len(errors), 1)
            self.assertEqual(errors[0]["code"], "missing-column")
            self.assertIn("missing column(s) {email}", errors[0]["message"])
            self.assertIn("required: {customer_id, email, name}", errors[0]["message"])

    def test_should_track_column_list_slice_in_function_contract(self) -> None:
        """Test that a list-slice (`df[["a", "b"]]`) registers both columns in the contract."""
        # arrange
        loaders_source = """
import pandas as pd

def load(path: str) -> pd.DataFrame:
    df = pd.read_csv(path, usecols=["a"])
    return df
"""
        steps_source = """
import pandas as pd

def preproc(df: pd.DataFrame) -> pd.DataFrame:
    slim = df[["a", "b"]]
    return slim
"""
        pipeline_source = """
from loaders import load
from steps import preproc

def process(path: str) -> None:
    df = load(path)
    result = preproc(df)
    print(result)
"""
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            (root / "loaders.py").write_text(loaders_source)
            (root / "steps.py").write_text(steps_source)
            (root / "pipeline.py").write_text(pipeline_source)
            (root / "pyproject.toml").write_text("[tool.typedframes]\nenabled = true\n")

            # act
            index_bytes = build_project_index(tmpdir)
            result = check_file(str(root / "pipeline.py"), index_bytes)
            errors = json.loads(result)

            # assert — loader only supplies 'a'; preproc's list-slice needs 'a' AND 'b'
            self.assertEqual(len(errors), 1)
            self.assertEqual(errors[0]["code"], "missing-column")
            self.assertIn("missing column(s) {b}", errors[0]["message"])

    def test_should_catch_narrowed_variable_misuse_locally_not_via_caller_contract(self) -> None:
        """Test that misusing a narrowed variable is caught locally, not folded into the contract."""
        # arrange
        source = """
import pandas as pd

def preproc(df: pd.DataFrame) -> pd.DataFrame:
    slim = df[["a", "b"]]
    z = slim["c"]
    return slim
"""
        with tempfile.NamedTemporaryFile(mode="w", suffix=".py", delete=False) as f:
            f.write(source)
            temp_file = f.name

        try:
            # act
            result = check_file(temp_file, None)
            errors = json.loads(result)

            # assert — caught directly, as an unknown-column bug local to preproc
            self.assertEqual(len(errors), 1)
            self.assertEqual(errors[0]["code"], "unknown-column")
            self.assertIn("'c'", errors[0]["message"])
            self.assertIn("{a, b}", errors[0]["message"])
        finally:
            Path(temp_file).unlink()

    def test_should_catch_column_access_nested_in_arithmetic_expression(self) -> None:
        """Test that a bad column access nested inside a binary expression is still validated."""
        # arrange
        source = """
from typedframes import BaseSchema, Column
from typedframes.pandas import PandasFrame

class Schema(BaseSchema):
    a = Column(type=int)
    b = Column(type=int)

def combine(df: PandasFrame[Schema]):
    return df["a"] + df["bad"]
"""
        with tempfile.NamedTemporaryFile(mode="w", suffix=".py", delete=False) as f:
            f.write(source)
            temp_file = f.name

        try:
            # act
            result = check_file(temp_file, None)
            errors = json.loads(result)

            # assert
            self.assertEqual(len(errors), 1)
            self.assertIn("'bad'", errors[0]["message"])
        finally:
            Path(temp_file).unlink()

    def test_should_catch_column_access_in_keyword_argument(self) -> None:
        """Test that a bad column access passed as a keyword argument is still validated."""
        # arrange
        source = """
from typedframes import BaseSchema, Column
from typedframes.pandas import PandasFrame

class Schema(BaseSchema):
    a = Column(type=int)

def enrich(df: PandasFrame[Schema]):
    return df.assign(doubled=df["bad"] * 2)
"""
        with tempfile.NamedTemporaryFile(mode="w", suffix=".py", delete=False) as f:
            f.write(source)
            temp_file = f.name

        try:
            # act
            result = check_file(temp_file, None)
            errors = json.loads(result)

            # assert
            self.assertEqual(len(errors), 1)
            self.assertIn("'bad'", errors[0]["message"])
        finally:
            Path(temp_file).unlink()
