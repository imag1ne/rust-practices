#![allow(dead_code)]

use std::sync::mpsc;
use std::thread;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

fn mpsc_bounded_cached(count: usize) -> usize {
    let (px, cx) = mpsc_bounded_cached::channel();

    {
        let px = px.clone();
        thread::spawn(move || {
            for i in 0..count {
                px.send(i).unwrap();
            }
        });
    }

    {
        let px = px.clone();
        thread::spawn(move || {
            for i in 0..count {
                px.send(i).unwrap();
            }
        });
    }

    thread::spawn(move || {
        for i in 0..count {
            px.send(i).unwrap();
        }
    });

    thread::spawn(move || {
        let mut sum = 0usize;

        while let Ok(i) = cx.recv() {
            sum += i;
        }

        sum
    })
    .join()
    .unwrap()
}

fn mpsc(count: usize) -> usize {
    let (sx, rx) = mpsc::channel();

    {
        let sx = sx.clone();
        thread::spawn(move || {
            for i in 0..count {
                sx.send(i).unwrap();
            }
        });
    }

    {
        let sx = sx.clone();
        thread::spawn(move || {
            for i in 0..count {
                sx.send(i).unwrap();
            }
        });
    }

    thread::spawn(move || {
        for i in 1..count {
            sx.send(i).unwrap();
        }
    });

    thread::spawn(move || {
        let mut sum = 0usize;

        while let Ok(i) = rx.recv() {
            sum += i;
        }

        sum
    })
    .join()
    .unwrap()
}

fn mpsc_bounded_cached_vs_mpsc(c: &mut Criterion) {
    let mut group = c.benchmark_group("mpsc_bounded_cached vs mpsc");

    for ref i in (8..=12).map(|n| 1 << n) {
        group.bench_with_input(BenchmarkId::new("mpsc_bounded_cached", i), i, |b, &i| {
            b.iter(|| mpsc_bounded_cached(i))
        });
        group.bench_with_input(BenchmarkId::new("mpsc", i), i, |b, &i| b.iter(|| mpsc(i)));
    }

    group.finish();
}

criterion_group!(benches, mpsc_bounded_cached_vs_mpsc,);
criterion_main!(benches);
