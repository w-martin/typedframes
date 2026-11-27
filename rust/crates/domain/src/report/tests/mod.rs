use super::*;

mod redaction;
mod sites;
mod snippet;

pub(super) fn linter_for(source: &str) -> Linter {
    let mut linter = Linter::new();
    linter.collect_suggestions = true;
    linter.suggest_display_path = "job.py".to_string();
    linter
        .check_file_internal(source, Path::new("job.py"))
        .unwrap();
    linter
}

pub(super) fn report(source: &str, var: &str) -> SiteReport {
    let linter = linter_for(source);
    let name = linter
        .untyped_sites
        .iter()
        .find(|s| s.var == var)
        .expect("no such site")
        .phrase
        .clone();
    report_for(source, &linter, &name).expect("site has a report")
}

// The redacted code before it is reduced, for tests of what redaction keeps and hides.
pub(super) fn unreduced(source: &str, var: &str) -> String {
    let linter = linter_for(source);
    let site = linter
        .untyped_sites
        .iter()
        .find(|s| s.var == var)
        .expect("no such site");
    let index = LineIndex::from_source_text(source);
    let offset = phrase::offset_of(source, &index, site.line, site.col).expect("offset");
    build_snippet(
        source,
        offset,
        linter.imported_function_libraries(),
        linter.imported_class_method_libraries(),
    )
    .expect("a snippet")
    .0
}

pub(super) const PRIVATE: &str = r#"import pandas as pd
from acme_internal import secret_loader


class PricingService:
    def run(self, customer_id):
        revenue = pd.read_csv("s3://acme/customers.csv", sep=";")
        total = revenue["gross_margin"].sum() + revenue["loss"].sum()
        self.audit(revenue)
        return total + customer_id

    def audit(self, frame):
        # TODO: ask about the quarterly numbers
        return frame.groupby("region").sum()
"#;
