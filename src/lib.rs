#![feature(test)]
#![deny(warnings)]
#![feature(asm)]
extern crate scoped_threadpool;
extern crate test;
extern crate rayon;
extern crate crossbeam;
extern crate core_affinity;
extern crate arrayvec;

#[macro_use]
extern crate lazy_static;

mod mypool;
pub mod mypool2;
pub mod mypool3;

#[cfg(test)]
use test::Bencher;
use arrayvec::ArrayVec;
#[cfg(test)]
use std::ops::DerefMut;
mod ptr_holder_1;
mod ptr_holder_2;


pub const PROB_SIZE: usize = 100000;
pub const THREADS: usize = 8;

pub fn run_rayon_scoped<T: Sync + Send, F: Fn(&mut T) + Send + Sync + 'static>(data: &mut [T], f: F, thread_count: usize) {
    let chunk_size = (data.len() + thread_count - 1) / thread_count;

    rayon::scope(|s| {
        for datachunk_items in data.chunks_mut(chunk_size) {
            s.spawn(|_| {
                for item in datachunk_items {
                    f(item);
                }
            });
        }
    });
}

pub fn run_scoped_threadpool<T: Sync + Send, F: Fn(&mut T) + Send + Sync + 'static>(data: &mut [T], pool: &mut scoped_threadpool::Pool, f: F) {
    let thread_count = pool.thread_count() as usize;
    let chunk_size = (data.len() + thread_count - 1) / thread_count;
    let fref = &f;

    pool.scoped(move |scope| {
        for datachunk_items in data.chunks_mut(chunk_size) {
            scope.execute(/*thread_index,*/ move || {
                for item in datachunk_items {
                    fref(item);
                }
            });
        }
    });
}

pub fn run_mypool<T: Send+Sync, F: Fn(&mut T) + Send + Sync + 'static>(data:&mut [T], pool: &mut mypool::Pool, f: F) {
    let thread_count = pool.thread_count() as usize;
    let chunk_size = (data.len() + thread_count - 1) / thread_count;
    let fref = &f;

    pool.scoped(move |scope| {
        for (thread_index, datachunk_items) in data.chunks_mut(chunk_size).enumerate() {
            scope.execute(thread_index, move || {
                for item in datachunk_items {
                    fref(item);
                }
            });
        }
    });
}


pub fn run_mypool2<T: Send+Sync, F: Fn(&mut T) + Send + Sync + 'static>(data: &mut [T], pool: &mut mypool2::Pool, f: F) {
    let thread_count = pool.thread_count() as usize;
    let chunk_size = (data.len() + thread_count-1) / thread_count;
    let fref = &f;


    let args: ArrayVec<[_; THREADS]> = data.chunks_mut(chunk_size).map(|datachunk_items| {
        let f = || {
            for item in datachunk_items {
                fref(item);
            }
        };
        f
    }).collect();
    pool.execute_all(args);
}


pub fn check_data(data:&Vec<u64>) {
    for (num,item) in data.iter().enumerate() {
        assert_eq!(num as u64 + 1,*item);
    }

}
pub fn make_data() -> Vec<u64> {
    let mut data = Vec::new();
    let data_size = PROB_SIZE as u64;
    for i in 0..data_size {
        data.push(i);
    }
    data
}


#[bench]
fn benchmark_non_threaded(bench: &mut Bencher) {
    let mut data = make_data();
    bench.iter(move || {
        for data in data.iter_mut() {
            *data += 1;
        }
    });
}

#[bench]
fn benchmark_scoped_threadpool(bench: &mut Bencher) {
    let mut pool = scoped_threadpool::Pool::new(THREADS as u32);
    let mut data = make_data();
    bench.iter(move || {
        run_scoped_threadpool(&mut data, &mut pool, |item: &mut u64| {
            *item += 1;
        });
    });
}


#[bench]
fn benchmark_rayon_scoped(bench: &mut Bencher) {
    let mut data = make_data();
    bench.iter(move || {
        run_rayon_scoped(&mut data, |item| {
            *item += 1;
        }, THREADS);
    });
}

#[bench]
fn benchmark_mypool(bench: &mut Bencher) {
    let mut pool = mypool::Pool::new(THREADS);
    let mut data = make_data();
    bench.iter(move || {
        run_mypool(&mut data,&mut pool, |item| {
            *item += 1;
        });
    });
}


#[bench]
fn benchmark_mypool2(bench: &mut Bencher) {
    let mut pool = mypool2::Pool::new(THREADS);
    let mut data = make_data();
    bench.iter(move || {
        run_mypool2(&mut data, &mut pool, |item| {
            *item += 1;
        });
    });
}


#[bench]
pub fn benchmark_aux_locking(bench: &mut Bencher) {
    let mut pool = mypool2::Pool::new(THREADS);
    let mut data = make_data();
    use parking_lot::Mutex;
    let thread_count = pool.thread_count();
    let chunk_size = (data.len() + thread_count-1) / thread_count;

    let aux:Vec<Mutex<u64>> = make_data().drain(..).map(|x|Mutex::new(x)).collect();


    let auxref = &aux;
    bench.iter(move || {

        let args: ArrayVec<[_; THREADS]> = data.chunks_mut(chunk_size).map(|datachunk_items| {
            let f = move|| {
                for item in datachunk_items.iter_mut() {
                    let mut guard = auxref[*item as usize].lock();
                    *guard.deref_mut() += 1;
                }
            };
            f
        }).collect();
        pool.execute_all(args);

    });
}

#[bench]
pub fn benchmark_aux_singlethreaded(bench: &mut Bencher) {
    let mut data = make_data();

    let mut aux:Vec<u64> = make_data();


    bench.iter(move || {

        let mut sum=0;
        for (idx,data) in data.iter_mut().enumerate() {
            sum+=*data;
            aux[idx] = aux[idx].wrapping_add(1);
        }
        sum
    });
}

#[bench]
pub fn benchmark_aux_rayon_locking(bench:&mut Bencher) {
    let mut data = make_data();
    let thread_count = THREADS;
    let chunk_size = (data.len() + thread_count - 1) / thread_count;
    use parking_lot::Mutex;
    let aux:Vec<Mutex<u64>> = make_data().drain(..).map(|x|Mutex::new(x)).collect();
    let auxref = &aux;
    bench.iter(move || {
        rayon::scope(|s| {
            for datachunk_items in data.chunks_mut(chunk_size) {
                s.spawn(|_| {
                    for item in datachunk_items {
                        let tid = *item as usize;
                        let mut guard = auxref[tid].lock();
                        *guard.deref_mut() += 1;
                    }
                });
            }
        });
    });
}

