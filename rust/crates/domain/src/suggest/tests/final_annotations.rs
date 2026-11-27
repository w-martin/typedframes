use super::*;

#[test]
fn test_should_add_final_to_an_existing_typing_import_in_sorted_position() {
    // arrange
    let source =
        "from typing import Any, Optional\n\nimport pandas as pd\n\n\nclass Repo:\n    A = \"a\"\n";

    // act
    let (_, edited) = final_fix(source, "Repo", "A").unwrap();

    // assert: one typing import, with `Final` where isort would put it
    assert!(
        edited.starts_with("from typing import Any, Final, Optional\n"),
        "{edited}"
    );
    assert_eq!(edited.matches("import Final").count(), 0, "{edited}");
}

#[test]
fn test_should_keep_a_lazy_typing_import_lazy_when_adding_final() {
    // arrange
    let source =
        "lazy from typing import Any\n\nlazy import pandas as pd\n\n\nclass Repo:\n    A = \"a\"\n";

    // act
    let (_, edited) = final_fix(source, "Repo", "A").unwrap();

    // assert
    assert!(
        edited.starts_with("lazy from typing import Any, Final\n"),
        "{edited}"
    );
    assert!(edited.contains("\nlazy import pandas as pd\n"), "{edited}");
    assert_eq!(edited.matches("import Final").count(), 0, "{edited}");
}

#[test]
fn test_should_put_final_after_constants_and_before_functions_like_ruff_isort() {
    for (names, expected) in [
        ("TYPE_CHECKING, Any", "TYPE_CHECKING, Any, Final"),
        ("TYPE_CHECKING", "TYPE_CHECKING, Final"),
        ("Any, cast", "Any, Final, cast"),
        ("Generic, TypeVar", "Final, Generic, TypeVar"),
        ("cast", "Final, cast"),
    ] {
        // arrange
        let source = format!("from typing import {names}\n\n\nclass Repo:\n    A = \"a\"\n");

        // act
        let (_, edited) = final_fix(&source, "Repo", "A").unwrap();

        // assert
        assert!(
            edited.starts_with(&format!("from typing import {expected}\n")),
            "{edited}"
        );
    }
}

#[test]
fn test_should_declare_a_plain_class_constant_final_and_import_final() {
    // arrange
    let source = "import pandas as pd\n\n\nclass Repo:\n    A = \"a\"\n";

    // act
    let (suggestion, edited) = final_fix(source, "Repo", "A").unwrap();

    // assert
    assert_eq!(suggestion.kind, "final");
    assert_eq!(suggestion.tier, "safe");
    assert_eq!(
        edited,
        "from typing import Final\n\nimport pandas as pd\n\n\nclass Repo:\n    A: Final = \"a\"\n"
    );
}

#[test]
fn test_should_wrap_an_existing_annotation_in_final() {
    // arrange
    let source = "from typing import Final\n\n\nclass Repo:\n    A: str = \"a\"\n";

    // act
    let (_, edited) = final_fix(source, "Repo", "A").unwrap();

    // assert: `Final` was already imported, so no second import.
    assert_eq!(
        edited,
        "from typing import Final\n\n\nclass Repo:\n    A: Final[str] = \"a\"\n"
    );
}

#[test]
fn test_should_reuse_the_typing_module_or_an_alias_instead_of_importing() {
    // arrange
    let module_import = "import typing as t\n\n\nclass Repo:\n    A = 1\n";
    let alias = "from typing import Final as F\n\n\nclass Repo:\n    A = 1\n";

    // act
    let (_, via_module) = final_fix(module_import, "Repo", "A").unwrap();
    let (_, via_alias) = final_fix(alias, "Repo", "A").unwrap();

    // assert
    assert!(via_module.contains("A: t.Final = 1"), "{via_module}");
    assert!(via_alias.contains("A: F = 1"), "{via_alias}");
}

#[test]
fn test_should_import_final_after_a_docstring_and_future_import() {
    // arrange
    let source =
        "\"\"\"Doc.\"\"\"\nfrom __future__ import annotations\n\n\nclass Repo:\n    A = 1\n";

    // act
    let (_, edited) = final_fix(source, "Repo", "A").unwrap();

    // assert
    assert!(
        edited.starts_with(
            "\"\"\"Doc.\"\"\"\nfrom __future__ import annotations\n\nfrom typing import Final\n"
        ),
        "{edited}"
    );
}

#[test]
fn test_should_import_final_before_the_first_statement_when_there_are_no_imports() {
    // arrange
    let source = "\"\"\"Doc.\"\"\"\n\n\nclass Repo:\n    A = 1\n";

    // act
    let (_, edited) = final_fix(source, "Repo", "A").unwrap();

    // assert
    assert!(
        edited.contains("\"\"\"Doc.\"\"\"\n\n\nfrom typing import Final\n\nclass Repo:"),
        "{edited}"
    );
}

#[test]
fn test_should_withhold_a_constant_not_assigned_once_in_the_class_body() {
    // arrange
    let inherited = "from lib import Base\n\n\nclass Repo(Base):\n    pass\n";
    let twice = "class Repo:\n    A = 1\n    A = 2\n";
    let chained = "class Repo:\n    A = B = 1\n";
    let unassigned = "class Repo:\n    A: int\n";

    // act / assert
    for source in [inherited, twice, chained, unassigned] {
        assert!(final_fix(source, "Repo", "A").is_err(), "{source}");
    }
}

#[test]
fn test_should_withhold_a_class_not_defined_once_at_the_top_level() {
    // arrange
    let source = "def make():\n    class Repo:\n        A = 1\n";

    // act
    let result = final_fix(source, "Repo", "A");

    // assert
    assert!(result.is_err());
}

#[test]
fn test_should_show_code_between_two_edits_as_context_in_the_diff() {
    // arrange
    let old = "a\nb\nc\nd\ne\nf\ng\nh\n";
    let new = "a\nX\nb\nc\nd\ne\nf\ng\nh\nY\n";

    // act
    let diff = render_unified_diff("f.py", old, new);

    // assert
    assert!(diff.contains("+X\n b\n c\n"), "{diff}");
    assert!(!diff.contains("-b"), "{diff}");
}

#[test]
fn test_should_declare_final_on_the_base_class_that_defines_the_constant() {
    // arrange
    let source = "class Base:\n    TARGET = \"y\"\n\n\nclass Mid(Base):\n    pass\n\n\nclass Repo(Mid):\n    pass\n";

    // act
    let (suggestion, edited) = final_fix(source, "Repo", "TARGET").unwrap();

    // assert
    assert_eq!(suggestion.line, 2);
    assert!(edited.contains("    TARGET: Final = \"y\""), "{edited}");
    assert!(
        suggestion.reason.contains("`Base`"),
        "{}",
        suggestion.reason
    );
}

#[test]
fn test_should_prefer_the_nearest_base_that_assigns_the_constant() {
    // arrange
    let source =
        "class Base:\n    T = 1\n\n\nclass Mid(Base):\n    T = 2\n\n\nclass Repo(Mid):\n    pass\n";

    // act
    let result = final_fix(source, "Repo", "T");

    // assert: Mid defines it, but it rebinds Base's, which Final on Mid does not touch.
    let (suggestion, _) = result.unwrap();
    assert_eq!(suggestion.line, 6);
}

#[test]
fn test_should_withhold_when_a_subclass_in_the_file_rebinds_the_constant() {
    // arrange
    let source = "class Base:\n    T = 1\n\n\nclass Child(Base):\n    T = 2\n";

    // act
    let result = final_fix(source, "Base", "T");

    // assert
    let reason = result.map(|_| ()).unwrap_err();
    assert!(reason.contains("`Child` rebinds `T`"), "{reason}");
}

#[test]
fn test_should_withhold_when_an_earlier_base_is_defined_in_another_file() {
    // arrange
    let source = "from lib import Mixin\n\n\nclass Base:\n    T = 1\n\n\nclass Repo(Mixin, Base):\n    pass\n";

    // act
    let result = final_fix(source, "Repo", "T");

    // assert
    assert!(result.is_err());
}

#[test]
fn test_should_insert_the_import_on_its_own_line_after_a_trailing_comment() {
    // arrange
    let source = "import pandas as pd  # noqa: F401\n\n\nclass Repo:\n    A = 1\n";

    // act
    let (_, edited) = final_fix(source, "Repo", "A").unwrap();

    // assert: the pragma stays on the import it belongs to.
    assert!(
        edited.starts_with("from typing import Final\n\nimport pandas as pd  # noqa: F401\n"),
        "{edited}"
    );
}

#[test]
fn test_should_insert_the_import_after_everything_sharing_the_last_import_line() {
    // arrange
    let source = "import os; x = 1\n\n\nclass Repo:\n    A = 1\n";
    let continued = "import os; x = (\n    1\n)\n\n\nclass Repo:\n    A = 1\n";

    // act
    let (_, edited) = final_fix(source, "Repo", "A").unwrap();
    let (_, edited_continued) = final_fix(continued, "Repo", "A").unwrap();

    // assert
    assert!(
        edited.starts_with("import os; x = 1\nfrom typing import Final\n"),
        "{edited}"
    );
    assert!(
        edited_continued.starts_with("import os; x = (\n    1\n)\nfrom typing import Final\n"),
        "{edited_continued}"
    );
}

#[test]
fn test_should_use_the_files_line_ending_for_the_import() {
    // arrange
    let source = "import os\r\n\r\n\r\nclass Repo:\r\n    A = 1\r\n";
    let bare = "\"\"\"Doc.\"\"\"\r\n\r\n\r\nclass Repo:\r\n    A = 1\r\n";

    // act
    let (_, edited) = final_fix(source, "Repo", "A").unwrap();
    let (_, edited_bare) = final_fix(bare, "Repo", "A").unwrap();

    // assert: no bare `\n` among the CRLFs.
    for text in [edited, edited_bare] {
        assert!(text.contains("from typing import Final\r\n"), "{text:?}");
        assert_eq!(
            text.matches('\n').count(),
            text.matches("\r\n").count(),
            "{text:?}"
        );
    }
}

#[test]
fn test_should_turn_a_class_var_annotation_into_final() {
    // arrange
    let source = "from typing import ClassVar, Final\n\n\nclass Repo:\n    A: ClassVar[str] = \"a\"\n    B: ClassVar = \"b\"\n";

    // act
    let (_, edited_a) = final_fix(source, "Repo", "A").unwrap();
    let (_, edited_b) = final_fix(source, "Repo", "B").unwrap();

    // assert
    assert!(edited_a.contains("A: Final[str] = \"a\""), "{edited_a}");
    assert!(edited_b.contains("B: Final = \"b\""), "{edited_b}");
}

#[test]
fn test_should_drop_the_class_var_import_once_nothing_uses_it() {
    // arrange
    let one = "from typing import ClassVar\n\nimport pandas as pd\n\n\nclass T:\n    A: ClassVar[list[str]] = [\"a\"]\n";
    let kept = "from typing import Any, ClassVar\n\n\nclass T:\n    A: ClassVar[list[str]] = [\"a\"]\n    B: ClassVar[list[str]] = [\"b\"]\n";
    let several = "from typing import ClassVar, Final\n\n\nclass T:\n    A: ClassVar[list[str]] = [\"a\"]\n    B: ClassVar[list[str]] = [\"b\"]\n";

    // act
    let (_, only) = final_fix(one, "T", "A").unwrap();
    let (_, shared) = final_fix(kept, "T", "A").unwrap();
    let both = without_orphaned_class_var(several, &several.replace("ClassVar[", "Final["));

    // assert
    assert!(only.starts_with("from typing import Final\n"), "{only}");
    assert!(shared.contains("import Any, ClassVar, Final"), "{shared}");
    assert!(both.starts_with("from typing import Final\n"), "{both}");
}

#[test]
fn test_should_leave_an_already_unused_class_var_import_alone() {
    // arrange
    let source = "from typing import ClassVar, Final\n\n\nclass T:\n    A: Final = 1\n";

    // act / assert
    assert_eq!(source, without_orphaned_class_var(source, source));
}

#[test]
fn test_should_render_two_distant_edits_as_two_hunks() {
    // arrange
    let old: String = (0..40).map(|i| format!("line {i}\n")).collect();
    let new =
        old.replacen("line 1\n", "line one\n", 1)
            .replacen("line 35\n", "line thirty-five\n", 1);

    // act
    let diff = render_unified_diff("f.py", &old, &new);

    // assert
    assert_eq!(2, diff.matches("@@ -").count(), "{diff}");
    assert!(diff.contains("-line 35\n+line thirty-five\n"), "{diff}");
    assert!(!diff.contains("line 15"), "{diff}");
}

#[test]
fn test_should_join_hunks_whose_context_meets() {
    // arrange
    let old: String = (0..20).map(|i| format!("line {i}\n")).collect();
    let new = old
        .replacen("line 5\n", "five\n", 1)
        .replacen("line 9\n", "nine\n", 1);

    // act
    let diff = render_unified_diff("f.py", &old, &new);

    // assert
    assert_eq!(1, diff.matches("@@ -").count(), "{diff}");
}

#[test]
fn test_should_withhold_when_final_is_already_something_else() {
    // arrange
    let assigned = "import os\n\nFinal = 3\n\n\nclass Repo:\n    A = 1\n";
    let imported = "from other import Final\n\n\nclass Repo:\n    A = 1\n";

    // act / assert
    for source in [assigned, imported] {
        let reason = final_fix(source, "Repo", "A").map(|_| ()).unwrap_err();
        assert!(reason.contains("`Final`"), "{reason}");
    }
}

#[test]
fn test_should_put_the_import_in_the_standard_library_group_in_sorted_position() {
    // arrange
    let cls = "\n\nclass Repo:\n    A = 1\n";
    let before_third_party =
        format!("import os\nfrom pathlib import Path\n\nimport pandas as pd\n{cls}");
    let between = format!("import os\nfrom collections import Counter\nfrom zlib import crc32\n\nimport pandas as pd\n{cls}");
    let first_from = format!(
        "from pathlib import Path\nfrom zipfile import ZipFile\n\nimport pandas as pd\n{cls}"
    );

    // act
    let (_, a) = final_fix(&before_third_party, "Repo", "A").unwrap();
    let (_, b) = final_fix(&between, "Repo", "A").unwrap();
    let (_, c) = final_fix(&first_from, "Repo", "A").unwrap();

    // assert: after the last smaller module, before the first larger one, no stray blank lines
    assert!(
        a.starts_with(
            "import os\nfrom pathlib import Path\nfrom typing import Final\n\nimport pandas"
        ),
        "{a}"
    );
    assert!(b.starts_with("import os\nfrom collections import Counter\nfrom typing import Final\nfrom zlib import crc32\n\nimport pandas"), "{b}");
    assert!(c.starts_with("from pathlib import Path\nfrom typing import Final\nfrom zipfile import ZipFile\n\nimport pandas"), "{c}");
    for edited in [a, b, c] {
        assert!(imports_in_order(&edited), "{edited}");
    }
}

#[test]
fn test_should_start_a_group_of_its_own_before_third_party_imports() {
    // arrange
    let source = "import numpy as np\nimport pandas as pd\n\nfrom myapp import util\n\n\nclass Repo:\n    A = 1\n";

    // act
    let (_, edited) = final_fix(source, "Repo", "A").unwrap();

    // assert
    assert!(
            edited.starts_with("from typing import Final\n\nimport numpy as np\nimport pandas as pd\n\nfrom myapp import util\n"),
            "{edited}"
        );
    assert!(imports_in_order(&edited));
}

#[test]
fn test_should_tell_ordered_import_blocks_from_unordered_ones() {
    // arrange / act / assert
    assert!(imports_in_order(
        "import os\nimport sys\n\nimport pandas as pd\n"
    ));
    assert!(!imports_in_order("import pandas as pd\nimport os\n"));
    assert!(!imports_in_order(
        "import pandas as pd\nfrom typing import Final\n"
    ));
    assert!(!imports_in_order(
        "import os\nfrom typing import Final\nfrom pathlib import Path\n"
    ));
    assert!(!imports_in_order("import os\n\nimport sys\n"));
}

#[test]
fn test_should_read_the_columns_of_a_selected_groupby_and_a_literal_rename() {
    // arrange
    let source = "items = pd.read_parquet(\"i.parquet\")\nitems[\"price\"] = items[\"price\"].fillna(items.groupby(\"category\")[\"price\"].transform(\"median\"))\nitems = items.rename(columns={\"quality_score\": \"qscore\"})\nprint(items[\"qscore\"].mean())\n";

    // act
    let suggestion = run(source, false).unwrap();

    // assert: the group key, the selected column and the renamed source are read; the new
    // name is not a column of the file
    assert_eq!(suggestion.columns, ["price", "category", "quality_score"]);
}

#[test]
fn test_should_read_the_sources_of_a_named_aggregation() {
    // arrange
    let source = "items = pd.read_parquet(\"i.parquet\")\nseg = items.groupby(\"category\").agg(avg=(\"price\", \"mean\"))\nprint(seg)\n";

    // act
    let suggestion = run(source, false).unwrap();

    // assert
    assert_eq!(suggestion.columns, ["category", "price"]);
}

#[test]
fn test_should_still_block_a_groupby_whose_result_reads_every_column() {
    // arrange
    let source = "items = pd.read_parquet(\"i.parquet\")\ntotals = items.groupby(\"category\").sum()\nprint(items[\"price\"])\n";

    // act
    let reason = run(source, false).unwrap_err();

    // assert
    assert!(reason.contains(".groupby()"), "{reason}");
}
