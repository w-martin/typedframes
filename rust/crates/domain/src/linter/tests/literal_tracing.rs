use super::*;

#[test]
fn test_should_trace_a_dict_variable_into_a_dataframe() {
    let (typed, errors) = typed_count(
            "import pandas as pd\n\ndata = {\"x\": [1], \"y\": [2]}\ndf = pd.DataFrame(data)\nprint(df[\"x\"])\nprint(df[\"nope\"])\n",
        );

    assert_eq!(typed, 1);
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'nope'") && errors[0].message.contains("{x, y}"));
}

#[test]
fn test_should_trace_a_list_of_dicts_variable_into_a_dataframe() {
    let (typed, errors) = typed_count(
            "import pandas as pd\n\nrows = [{\"x\": 1}, {\"x\": 2, \"y\": 3}]\ndf = pd.DataFrame(rows)\nprint(df[\"nope\"])\n",
        );

    assert_eq!(typed, 1);
    assert!(errors[0].message.contains("{x, y}"), "errors: {errors:?}");
}

#[test]
fn test_should_trace_a_column_list_variable_through_columns() {
    let (typed, errors) = typed_count(
            "import pandas as pd\n\ncols = [\"x\", \"y\"]\ndf = pd.DataFrame(rows, columns=cols)\nprint(df[\"nope\"])\n",
        );

    assert_eq!(typed, 1);
    assert!(errors[0].message.contains("{x, y}"), "errors: {errors:?}");
}

#[test]
fn test_should_trace_a_column_list_variable_through_usecols() {
    let (typed, _) = typed_count(
        "import pandas as pd\n\ncols = [\"x\", \"y\"]\ndf = pd.read_csv(\"f.csv\", usecols=cols)\n",
    );
    assert_eq!(typed, 1);
}

#[test]
fn test_should_trace_literal_variables_for_polars_too() {
    let (typed, _) = typed_count(
            "import polars as pl\n\ncols = [\"x\"]\ndata = {\"x\": [1]}\na = pl.read_parquet(\"f.parquet\", columns=cols)\nb = pl.DataFrame(data)\n",
        );
    assert_eq!(typed, 2);
}

#[test]
fn test_should_trace_a_variable_used_by_several_loaders() {
    let (typed, _) = typed_count(
            "import pandas as pd\n\ncols = [\"x\"]\na = pd.read_csv(\"a.csv\", usecols=cols)\nb = pd.read_csv(\"b.csv\", usecols=cols)\n",
        );
    assert_eq!(typed, 2);
}

#[test]
fn test_should_not_trace_a_dict_mutated_by_a_subscript_store() {
    let (typed, _) = typed_count(
        "import pandas as pd\n\ndata = {\"x\": [1]}\ndata[\"z\"] = [2]\ndf = pd.DataFrame(data)\n",
    );
    assert_eq!(typed, 0);
}

#[test]
fn test_should_not_trace_a_dict_mutated_by_update() {
    let (typed, _) = typed_count(
        "import pandas as pd\n\ndata = {\"x\": [1]}\ndata.update(extra)\ndf = pd.DataFrame(data)\n",
    );
    assert_eq!(typed, 0);
}

#[test]
fn test_should_not_trace_a_literal_list_that_is_appended_to() {
    let (typed, _) = typed_count(
            "import pandas as pd\n\nrows = [{\"x\": 1}]\nrows.append({\"y\": 2})\ndf = pd.DataFrame(rows)\n",
        );
    assert_eq!(typed, 0);
}

#[test]
fn test_should_not_trace_a_name_bound_twice() {
    let (typed, _) = typed_count(
            "import pandas as pd\n\ndata = {\"x\": [1]}\ndata = {\"y\": [1]}\ndf = pd.DataFrame(data)\n",
        );
    assert_eq!(typed, 0);
}

#[test]
fn test_should_not_trace_a_name_that_is_also_aliased_or_passed_on() {
    // arrange: a read that cannot change it (`len(rows)`) is fine ...
    let (typed, _) = typed_count(
        "import pandas as pd\n\nrows = [{\"x\": 1}]\nprint(len(rows))\ndf = pd.DataFrame(rows)\n",
    );
    assert_eq!(typed, 1);
    // ... one that could is not
    let (typed, _) = typed_count(
        "import pandas as pd\n\ndata = {\"x\": [1]}\nalias = data\ndf = pd.DataFrame(data)\n",
    );
    assert_eq!(typed, 0);
    let (typed, _) = typed_count(
        "import pandas as pd\n\ndata = {\"x\": [1]}\nhelper(data)\ndf = pd.DataFrame(data)\n",
    );
    assert_eq!(typed, 0);
}

#[test]
fn test_should_not_trace_a_dict_that_does_not_name_every_column() {
    let (typed, _) = typed_count(
        "import pandas as pd\n\ndata = {\"x\": [1], **extra}\ndf = pd.DataFrame(data)\n",
    );
    assert_eq!(typed, 0);
    let (typed, _) = typed_count(
        "import pandas as pd\n\ndata = {\"x\": [1], key: [2]}\ndf = pd.DataFrame(data)\n",
    );
    assert_eq!(typed, 0);
}

#[test]
fn test_should_not_trace_an_empty_literal() {
    let (typed, _) = typed_count("import pandas as pd\n\ndata = {}\ndf = pd.DataFrame(data)\n");
    assert_eq!(typed, 0);
}

#[test]
fn test_should_not_trace_a_name_that_is_also_a_parameter_in_the_same_scope() {
    // the parameter binds `cols` inside `f`, so its value there is whatever the caller
    // passes ...
    let (typed, _) = typed_count(
            "import pandas as pd\n\ncols = [\"x\"]\n\n\ndef f(cols):\n    return pd.read_csv(\"f.csv\", usecols=cols)\n",
        );
    assert_eq!(typed, 0);
    // ... but a parameter of ANOTHER function leaves the module-level `cols` alone
    let (typed, _) = typed_count(
            "import pandas as pd\n\ncols = [\"x\"]\n\n\ndef f(cols):\n    return cols\n\n\ndf = pd.read_csv(\"f.csv\", usecols=cols)\n",
        );
    assert_eq!(typed, 1);
}

#[test]
fn test_should_not_confuse_a_column_list_with_rows() {
    // a list of names handed to `DataFrame(...)` positionally is row DATA, and a
    // dict handed to `columns=` is not a list of names.
    let (typed, _) =
        typed_count("import pandas as pd\n\nnames = [\"a\", \"b\"]\ndf = pd.DataFrame(names)\n");
    assert_eq!(typed, 0);
    let (typed, _) = typed_count(
        "import pandas as pd\n\ndata = {\"x\": [1]}\ndf = pd.DataFrame(rows, columns=data)\n",
    );
    assert_eq!(typed, 0);
}

#[test]
fn test_should_let_a_dataframe_method_read_a_column_list_but_not_an_unknown_function() {
    // `base.drop(columns=cols)` only reads `cols`, as every DataFrame API method does ...
    let (typed, _) = typed_count(
            "import pandas as pd\n\ncols = [\"x\"]\nother = base.drop(columns=cols)\ndf = pd.read_csv(\"f.csv\", usecols=cols)\n",
        );
    assert_eq!(typed, 1);
    // ... an unknown function might keep or change it
    let (typed, _) = typed_count(
            "import pandas as pd\n\ncols = [\"x\"]\nother = helper(columns=cols)\ndf = pd.read_csv(\"f.csv\", usecols=cols)\n",
        );
    assert_eq!(typed, 0);
    // and a list-mutator name is never treated as a DataFrame method
    let (typed, _) = typed_count(
            "import pandas as pd\n\ncols = [\"x\"]\nresults.append(cols)\ndf = pd.read_csv(\"f.csv\", usecols=cols)\n",
        );
    assert_eq!(typed, 0);
}

#[test]
fn test_should_trace_a_binding_annotated_with_a_plain_type_only() {
    let (typed, _) =
        typed_count("import pandas as pd\n\ndata: dict = {\"x\": [1]}\ndf = pd.DataFrame(data)\n");
    assert_eq!(typed, 1);
    // an annotation naming anything else says the name is not just its value
    let (typed, _) = typed_count(
        "import pandas as pd\n\ndata: Schema = {\"x\": [1]}\ndf = pd.DataFrame(data)\n",
    );
    assert_eq!(typed, 0);
}

#[test]
fn test_should_keep_tracking_a_dataframe_declared_by_annotation_over_a_none_value() {
    let source = "import polars as pl\nfrom typing import Annotated\nfrom typedframes import BaseSchema, Column\n\n\nclass S(BaseSchema):\n    price = Column(type=float)\n\n\ndf: Annotated[pl.DataFrame, S] = None\nresult = df.select(\"cost\")\nalias = df\nbad = alias.select(\"nope\")\n";
    let (_, errors) = typed_count(source);
    assert!(
        errors.iter().any(|e| e.message.contains("'cost'")),
        "errors: {errors:?}"
    );
}

#[test]
fn test_should_not_trace_a_name_declared_global() {
    let (typed, _) = typed_count(
            "import pandas as pd\n\ndata = {\"x\": [1]}\n\n\ndef f():\n    global data\n    data = {\"y\": [1]}\n\n\ndf = pd.DataFrame(data)\n",
        );
    assert_eq!(typed, 0);
}
