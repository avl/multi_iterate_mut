use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId};

use multi_iterate_mut::SimpleIterateMut;
use multi_iterate_mut::mypool2::Pool;
use multi_iterate_mut::make_data;
use multi_iterate_mut::THREADS;


pub fn pool2_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("pool2");
    for threads in [1usize,2].iter() {
        group.bench_with_input(BenchmarkId::from_parameter(threads), threads, |b, &threads|
            {
                let mut pool = Pool::new(threads);
                let mut data = make_data();
                let mut simple = SimpleIterateMut {
                    data: &mut data,
                };

                b.iter(|| {
                    simple.run_mypool2(&mut pool, |item| {
                        item.item += 1;
                    });
                })
            }
        );
    }
    group.finish();
}


pub fn rayon_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("rayon");
    for threads in [1,2].iter() {
        group.bench_with_input(BenchmarkId::from_parameter(threads), threads, |b, &threads|
            {

                let mut data = make_data();
                let mut simple = SimpleIterateMut {
                    data: &mut data,
                };

                b.iter(|| {
                    simple.run_rayon_scoped( |item| {
                        item.item += 1;
                    },threads);
                })
            }
        );
    }
    group.finish();
}


criterion_group!(pool2_benches, pool2_benchmark);
criterion_group!(rayon_benches, rayon_benchmark);
criterion_main!(pool2_benches, rayon_benches);
