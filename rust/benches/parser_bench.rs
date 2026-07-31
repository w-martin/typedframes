use criterion::{criterion_group, criterion_main, Criterion};
use std::path::Path;

use _rust_checker::Linter;

fn bench_check_file(c: &mut Criterion) {
    let source = include_str!("../../../examples/features/typedframes_example.py");
    let path = Path::new("examples/features/typedframes_example.py");

    c.bench_function("check_file_internal", |b| {
        b.iter(|| {
            let mut linter = Linter::new();
            linter
                .check_file_internal(source, path)
                .expect("parse failed");
        });
    });
}

criterion_group!(benches, bench_check_file);
criterion_main!(benches);
