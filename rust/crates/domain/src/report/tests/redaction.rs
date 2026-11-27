use super::*;

#[test]
fn test_should_keep_none_of_the_original_names_or_strings() {
    // act
    let snippet = unreduced(PRIVATE, "revenue");

    // assert
    for secret in [
        "acme",
        "PricingService",
        "customer",
        "revenue",
        "gross_margin",
        "loss",
        "region",
        "quarterly",
        "audit",
        "secret_loader",
        "s3://",
        "TODO",
    ] {
        assert!(
            !snippet.contains(secret),
            "{secret} leaked into:\n{snippet}"
        );
    }
    assert!(snippet.contains("pd.read_csv("), "{snippet}");
    assert!(snippet.contains("sep=\"c"), "{snippet}");
    assert!(snippet.contains(".groupby("), "{snippet}");
}

#[test]
fn test_should_keep_library_names_where_the_library_uses_them_and_rename_ordinary_variables() {
    // arrange
    let source = "import numpy as np\nimport pandas as pd\n\n\ndef run(tenants: np.ndarray, value):\n    raw = pd.read_csv(\"x.csv\")\n    value = np.percentile(tenants, 2, axis=0)\n    return raw.fillna(value=value), raw.shape\n";

    // act
    let snippet = unreduced(source, "raw");

    // assert
    assert!(snippet.contains("np.ndarray"), "{snippet}");
    assert!(snippet.contains("np.percentile("), "{snippet}");
    assert!(snippet.contains(".fillna(value="), "{snippet}");
    assert!(snippet.contains(".shape"), "{snippet}");
    assert!(!snippet.contains("tenants"), "{snippet}");
    assert!(snippet.contains("v2 = np.percentile("), "{snippet}");
    assert!(!snippet.contains("value ="), "{snippet}");
}

#[test]
fn test_should_rename_user_definitions_and_values_that_share_a_library_name() {
    // arrange: `load`, `Config` and `Catalog` are numpy or polars names too.
    let source = "import pandas as pd\n\n\nclass Catalog:\n    pass\n\n\ndef load(path):\n    return pd.read_csv(path)\n\n\ndef run():\n    Config = Catalog\n    raw = pd.read_csv(\"x.csv\")\n    print(load(raw))\n    return raw[\"a\"], Config\n";

    // act
    let snippet = unreduced(source, "raw");

    // assert
    for secret in ["Catalog", "Config", "load"] {
        assert!(
            !snippet.contains(secret),
            "{secret} leaked into:\n{snippet}"
        );
    }
    assert!(snippet.contains("pd.read_csv("), "{snippet}");
}

#[test]
fn test_should_keep_a_name_imported_from_a_library_wherever_it_is_used() {
    // arrange
    let source = "from pandas import DataFrame\nimport pandas as pd\n\n\ndef run(rows) -> DataFrame:\n    raw = pd.read_csv(\"x.csv\")\n    return DataFrame(raw[\"a\"])\n";

    // act
    let snippet = unreduced(source, "raw");

    // assert
    assert!(
        snippet.contains("from pandas import DataFrame"),
        "{snippet}"
    );
    assert!(snippet.contains("-> DataFrame:"), "{snippet}");
    assert!(snippet.contains("return DataFrame("), "{snippet}");
}

#[test]
fn test_should_not_leak_through_fstrings_docstrings_decorators_or_bases() {
    // arrange
    let source = r#"import pandas as pd
import acme_internal


@acme_internal.register("acme-jobs")
class NightlyJob(acme_internal.BaseJob):
    """Loads the acme pricing numbers."""

    def run(self, tenant):
        """Runs for one tenant."""
        path = f"/data/acme/{tenant}/orders_{self.stamp:%Y}.csv"
        orders = pd.read_csv(path)
        picked = {"acme_key": lambda row: row.acme_field}
        orders["acme_total"] = orders["acme_price"] * 2
        return orders.acme_status, picked
"#;

    // act
    let report = report(source, "orders");

    // assert
    let snippet = report.snippet.expect("a snippet was produced");
    for secret in [
        "acme", "tenant", "Nightly", "orders", "stamp", "pricing", "/data", "status",
    ] {
        assert!(
            !snippet.contains(secret),
            "{secret} leaked into:\n{snippet}"
        );
    }
}

#[test]
fn test_should_rename_the_same_thing_the_same_way() {
    // act
    let snippet = unreduced(PRIVATE, "revenue");

    // assert: the frame is renamed once and used under that name
    let first_line = snippet
        .lines()
        .find(|l| l.contains("read_csv"))
        .expect("the load is kept");
    let renamed = first_line.trim().split(' ').next().unwrap().to_string();
    assert!(
        snippet.matches(&format!("{renamed}.")).count()
            + snippet.matches(&format!("{renamed}[")).count()
            >= 2,
        "{snippet}"
    );
}

#[test]
fn test_should_redact_template_strings_numbers_and_f_string_conversions() {
    // arrange
    let source = r#"import pandas as pd


def run(who):
    path = f"/data/{who!r}/orders.csv"
    note = t"secret template {who} text_secret"
    limit = 12345678
    mask = 0xDEAD_BEEF
    rate = 0.0125
    raw = pd.read_csv(path)
    return raw["a"], note, limit, mask, rate, 0, 1
"#;

    // act
    let snippet = unreduced(source, "raw");

    // assert
    for secret in [
        "secret",
        "text_secret",
        "12345678",
        "DEAD",
        "0.0125",
        "orders",
        "data",
    ] {
        assert!(
            !snippet.contains(secret),
            "{secret} leaked into:\n{snippet}"
        );
    }
    assert!(snippet.contains("!r}"), "{snippet}");
    assert!(snippet.contains(", 0, 1"), "{snippet}");
}

#[test]
fn test_should_not_leak_a_column_name_containing_a_quote_through_the_verdict() {
    // arrange
    let source = "import pandas as pd\n\n\ndef run():\n    raw = pd.read_json(\"x.json\")\n    print(raw[\"o'brien_secret\"], raw[\"amount\"])\n";

    // act
    let report = report(source, "raw");

    // assert
    assert_eq!(report.verdict.outcome, "suggested");
    let text = format!("{} {:?}", report.verdict.reason, report.verdict.notes);
    assert!(
        !text.contains("brien") && !text.contains("secret"),
        "{text}"
    );
    assert!(report.snippet.is_some(), "{:?}", report.refusal);
}

#[test]
fn test_should_keep_option_strings_only_as_the_value_of_an_option_keyword() {
    // arrange
    let source = r#"import pandas as pd


def run(other):
    raw = pd.read_csv("x.csv")
    merged = raw.merge(other, how="left")
    return merged["left"], {"count": raw["first"]}
"#;

    // act
    let snippet = report(source, "raw").snippet.expect("a snippet");

    // assert
    assert!(snippet.contains("how=\"left\""), "{snippet}");
    assert_eq!(snippet.matches("\"left\"").count(), 1, "{snippet}");
    assert!(
        !snippet.contains("\"count\"") && !snippet.contains("\"first\""),
        "{snippet}"
    );
}

#[test]
fn test_should_rename_dunder_names_that_are_not_standard() {
    // arrange
    let source = "import pandas as pd\n\n\ndef run():\n    __acme_secret__ = 1\n    raw = pd.read_csv(\"x.csv\")\n    return raw[\"a\"], __acme_secret__\n";

    // act
    let snippet = report(source, "raw").snippet.expect("a snippet");

    // assert
    assert!(!snippet.contains("acme"), "{snippet}");
}

#[test]
fn test_should_copy_only_syntax_tokens_verbatim() {
    for kind in [
        "Lpar",
        "Dot",
        "Equal",
        "Def",
        "Return",
        "Newline",
        "Indent",
        "FStringStart",
        "TStringEnd",
    ] {
        assert!(copies_verbatim(kind), "{kind}");
    }
    for kind in [
        "Name",
        "String",
        "Int",
        "Float",
        "Complex",
        "FStringMiddle",
        "TStringMiddle",
        "Comment",
        "IpyEscapeCommand",
        "Unknown",
        "Question",
        "SomethingNew",
    ] {
        assert!(!copies_verbatim(kind), "{kind}");
    }
}

#[test]
fn test_should_keep_only_zero_and_one_as_numbers() {
    assert_eq!(masked_number("Int", "0"), "0");
    assert_eq!(masked_number("Int", "1"), "1");
    assert_eq!(masked_number("Int", "42"), "2");
    assert_eq!(masked_number("Int", "0xDEAD_BEEF"), "2");
    assert_eq!(masked_number("Float", "1.0"), "1.0");
    assert_eq!(masked_number("Float", "0.0125"), "2.5");
    assert_eq!(masked_number("Complex", "3j"), "2j");
}

#[test]
fn test_should_keep_a_snippet_for_common_operators_and_keywords() {
    // arrange: if a common token kind were missing from the verbatim list this would refuse
    let source = r#"import pandas as pd


def run(items, flag):
    raw = pd.read_csv("x.csv")
    total = 0
    for item in items:
        if item is not None and flag or not flag:
            total += item ** 2 // 3 % 5 << 1 >> 1 & 1 | 0 ^ 1
    try:
        pick = [x for x in items if x >= 1 and x <= 9 and x != 3 and x == 4]
        lam = lambda y: -y @ y
        while total < 100:
            total -= 1
            break
    except ValueError as err:
        raise RuntimeError from err
    finally:
        pass
    with open("f") as handle:
        value = (yield)
    assert total > 0, "m"
    return raw["a"], pick, lam, total, handle, value, ..., ~total
"#;

    // act
    let report = report(source, "raw");

    // assert
    assert!(report.snippet.is_some(), "{:?}", report.refusal);
}
