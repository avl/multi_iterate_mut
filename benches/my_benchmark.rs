use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId};

use multi_iterate_mut::SimpleIterateMut;
use multi_iterate_mut::mypool2::Pool;
use multi_iterate_mut::make_data;
use multi_iterate_mut::THREADS;


pub fn criterion_benchmark(c: &mut Criterion) {

    let mut group = c.benchmark_group("pool2");
    for threads in 1..12usize {

        group.bench_with_input(BenchmarkId::from_parameter(threads), &threads, |b,&threads|
            {
                let mut pool = Pool::new(threads);
                let mut data = make_data();
                let mut simple = SimpleIterateMut {
                    data : &mut data,
                };

                b.iter(||{
                    simple.run_mypool2(&mut pool,|item|{
                        item.item += 1;
                    });
                })
            }
        );
    }
    group.finish();
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
