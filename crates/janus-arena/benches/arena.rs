//! Throughput benchmark for the generational arena.
//!
//! Perf is measured from commit one (the engine's speed pillar). These numbers
//! feed the CI perf-budget gate once a baseline is committed.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use janus_arena::Arena;

const N: usize = 10_000;

fn bench_arena(c: &mut Criterion) {
    c.bench_function("arena/insert_10k", |b| {
        b.iter(|| {
            let mut a: Arena<u32> = Arena::with_capacity(N);
            for i in 0..N {
                black_box(a.insert(black_box(i as u32)));
            }
            a
        });
    });

    c.bench_function("arena/get_10k", |b| {
        let mut a: Arena<u32> = Arena::with_capacity(N);
        let ids: Vec<_> = (0..N).map(|i| a.insert(i as u32)).collect();
        b.iter(|| {
            let mut sum = 0u64;
            for &id in &ids {
                sum += u64::from(*black_box(a.get(black_box(id)).unwrap()));
            }
            sum
        });
    });

    // Churn: insert + remove in equal measure, exercising the free list.
    c.bench_function("arena/churn_10k", |b| {
        b.iter(|| {
            let mut a: Arena<u32> = Arena::new();
            let mut ids = Vec::with_capacity(N);
            for i in 0..N {
                ids.push(a.insert(i as u32));
            }
            for id in ids.drain(..) {
                black_box(a.remove(black_box(id)));
            }
            a
        });
    });
}

criterion_group!(benches, bench_arena);
criterion_main!(benches);
