//! Throughput benchmark for the string interner.
//!
//! Perf is measured from commit one (the engine's speed pillar). These numbers
//! feed the CI perf-budget gate once a baseline is committed.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use janus_atom::Interner;

/// A small, repetition-heavy corpus resembling HTML tag/attribute names — the
/// realistic interner workload (few distinct strings, interned constantly).
const CORPUS: &[&str] = &[
    "div", "span", "a", "p", "ul", "li", "img", "class", "id", "href", "src", "style", "div",
    "span", "a", "section", "header", "footer", "nav", "button", "input", "class", "id", "div",
];

fn bench_intern(c: &mut Criterion) {
    c.bench_function("intern/mixed_corpus", |b| {
        b.iter(|| {
            let mut interner = Interner::new();
            for s in CORPUS {
                black_box(interner.intern(black_box(s)));
            }
            interner
        });
    });

    c.bench_function("intern/resolve_hot", |b| {
        let mut interner = Interner::new();
        let atoms: Vec<_> = CORPUS.iter().map(|s| interner.intern(s)).collect();
        b.iter(|| {
            for &atom in &atoms {
                black_box(interner.resolve(black_box(atom)));
            }
        });
    });
}

criterion_group!(benches, bench_intern);
criterion_main!(benches);
