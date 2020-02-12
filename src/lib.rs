#![feature(test)]
//#![deny(warnings)]

extern crate scoped_threadpool;
extern crate test;
extern crate rayon;
extern crate crossbeam;
extern crate core_affinity;
extern crate arrayvec;

mod mypool;
mod mypool2;
use test::Bencher;
use arrayvec::ArrayVec;

pub struct UsizeExampleItem {
    item : usize,
}

pub struct SimpleIterateMut<'a,T> {
    data : &'a mut [T],
}

impl<'a,T:Send> SimpleIterateMut<'a,T> {
    pub fn new(data:&'a mut Vec<T>) -> SimpleIterateMut<'a,T> {
        SimpleIterateMut {
            data: &mut data[..]
        }
    }
    pub fn run_mypool<'b,F:Fn(&mut T)+Send+Sync+'static>(&'b mut self, pool: &mut mypool::Pool,f:F) {
        let thread_count = pool.thread_count() as usize;
        let chunk_size = (self.data.len() + thread_count-1)/thread_count;
        let fref=&f;

        pool.scoped(move|scope|{

            for (thread_index,datachunk_items) in self.data.chunks_mut(chunk_size).enumerate() {
                scope.execute(thread_index, move||{
                    for item in datachunk_items {
                        fref(item);
                    }
                });
            }
        });
    }
    pub fn run_mypool2<'b,F:Fn(&mut T)+Send+Sync+'static>(&'b mut self, pool: &mut mypool2::Pool,f:F) {
        let thread_count = pool.thread_count() as usize;
        let chunk_size = (self.data.len() + thread_count-1)/thread_count;
        let fref=&f;


        let args:ArrayVec<[_;THREADS]> = self.data.chunks_mut(chunk_size).map(|datachunk_items|{
            let f = ||{
                for item in datachunk_items {
                    fref(item);
                }
            };
            f
        }).collect();
        pool.execute_all(&args);

    }
    pub fn run_scoped_threadpool<'b,F:Fn(&mut T)+Send+Sync+'static>(&'b mut self, pool: &mut scoped_threadpool::Pool,f:F) {
        let thread_count = pool.thread_count() as usize;
        let chunk_size = (self.data.len() + thread_count-1)/thread_count;
        let fref=&f;

        pool.scoped(move|scope|{

            for (thread_index,datachunk_items) in self.data.chunks_mut(chunk_size).enumerate() {
                scope.execute(/*thread_index,*/ move||{
                    for item in datachunk_items {
                        fref(item);
                    }
                });
            }
        });
    }
    pub fn run_rayon_scoped<'b,F:Fn(&mut T)+Send+Sync+'static>(&'b mut self,f:F, thread_count:usize) {

        let chunk_size = (self.data.len() + thread_count-1)/thread_count;
        let fref=&f;

        rayon::scope(|s| {
            for (thread_index,datachunk_items) in self.data.chunks_mut(chunk_size).enumerate() {
                s.spawn(|_| {
                    for item in datachunk_items {
                        fref(item);
                    }
                });
            }
        });

    }
}

const PROB_SIZE:usize = 100_000;
const THREADS:usize = 12;

#[bench]
fn benchmark_non_threaded(bench:&mut Bencher) {
    let mut data = Vec::new();
    let data_size = PROB_SIZE;
    for i in 0..data_size {
        data.push(UsizeExampleItem {
            item:i,
        });
    }
    bench.iter(move||{


        //for (aux1,data) in &mut aux1.iter_mut().zip(data.iter_mut()) {
        for data in data.iter_mut() {
            data.item += 1;//data.item; //((data.item as f64*0.001).cos().cos().cos()*100.0) as usize;
        }
    });
}

#[bench]
fn benchmark_scoped_threadpool(bench:&mut Bencher) {
    let mut pool = scoped_threadpool::Pool::new(THREADS as u32);
    let mut data = Vec::new();
    let data_size = PROB_SIZE;
    for i in 0..data_size {
        data.push(UsizeExampleItem {
            item:i,
        });
    }
    bench.iter(move||{
        let mut simple = SimpleIterateMut {
            data : &mut data,
        };

        simple.run_scoped_threadpool(&mut pool,|item|{
            item.item += 1;
        });

    });
}



#[bench]
fn benchmark_rayon_scoped(bench:&mut Bencher) {
    let mut data = Vec::new();
    let data_size = PROB_SIZE;
    for i in 0..data_size {
        data.push(UsizeExampleItem {
            item:i,
        });
    }
    bench.iter(move||{
        let mut simple = SimpleIterateMut {
            data : &mut data,
        };

        simple.run_rayon_scoped(|item|{
            item.item += 1;
        }, THREADS);

    });
}
#[bench]
fn benchmark_mypool(bench:&mut Bencher) {
    let mut pool = mypool::Pool::new(THREADS);
    let mut data = Vec::new();
    let data_size = PROB_SIZE;
    for i in 0..data_size {
        data.push(UsizeExampleItem {
            item:i,
        });
    }
    bench.iter(move||{
        let mut simple = SimpleIterateMut {
            data : &mut data,
        };

        simple.run_mypool(&mut pool,|item|{
            item.item += 1;
        });

    });
}



#[bench]
fn benchmark_mypool2(bench:&mut Bencher) {
    let mut pool = mypool2::Pool::new(THREADS);
    let mut data = Vec::new();
    let data_size = PROB_SIZE;
    for i in 0..data_size {
        data.push(UsizeExampleItem {
            item:i,
        });
    }
    bench.iter(move||{
        let mut simple = SimpleIterateMut {
            data : &mut data,
        };

        simple.run_mypool2(&mut pool,|item|{
            item.item += 1;
        });

    });
}




