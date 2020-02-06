#![feature(test)]

use core::any::Any;
use std::marker::PhantomData;
use scoped_threadpool::Pool;

#[repr(align(8))]
struct FuncThreadData<F> {
    data: Vec<Vec<(usize,F)>>
}


struct MultiIterateMut<'a,'b,T, A1, F:for <'r> FnOnce(&'r mut A1)> {
    data : &'a mut Vec<T>,
    aux1 : &'b mut Vec<A1>,
    funcs: Vec<FuncThreadData<F>>
}



struct MultiIterateContext<'a,T, A1,F:for <'r> FnOnce(&'r mut A1)> {
    chunk_size:usize,
    mutator_closures:&'a mut Vec<Vec<(usize, F)>>,
    p1:PhantomData<T>,
    p2:PhantomData<A1>
}

impl<'a,'b,T:Send,A1:Send+Sync,F:for <'r> FnOnce(&'r mut A1) + Send > MultiIterateMut<'a,'b,T,A1,F> {

    #[inline(always)]
    pub fn run<L:Fn(&mut T, &mut MultiIterateContext<T,A1,F>, &Vec<A1>) + Send+Sync>(&mut self, pool: &mut Pool, l:L) {
        let pool_size = pool.thread_count() as usize;
        let chunk_size = (self.data.len() + pool_size - 1) / pool_size;

        while self.funcs.len() < pool_size {
            self.funcs.push(FuncThreadData{data:Vec::new()});
        }
        for funcvec in &mut self.funcs {
            while funcvec.data.len() < pool_size {
                funcvec.data.push(Vec::new());
            }
            for item in funcvec.data.iter_mut() {
                item.clear();
            }
        }
        {
            let aux1ref = &self.aux1;
            let selfdata = &mut self.data;
            let selffuncs = &mut self.funcs;
            pool.scoped(|scope| {
                for (chunk_items, mutators) in &mut selfdata.chunks_mut(chunk_size).zip(selffuncs.iter_mut()) {
                    let l_ref = &l;
                    let mut t = MultiIterateContext {
                        chunk_size: chunk_size,
                        mutator_closures: &mut mutators.data,
                        p1: PhantomData,
                        p2: PhantomData
                    };

                    scope.execute(move || {
                        for item in chunk_items {
                            l_ref(item, &mut t, aux1ref);
                        }
                    });
                }
            });
        }

        let mut pass_slices:Vec<Vec<&mut Vec<(usize,F)>>> = Vec::new();
        for _ in 0..pool_size {
            pass_slices.push(Vec::new());
        }
        for thread_output in self.funcs.iter_mut() {

            for (pass_index,pass_data_item) in thread_output.data.iter_mut().enumerate() {
                pass_slices[pass_index].push(pass_data_item);
            }

        }
        let aux_size = self.aux1.len();
        let mut cur_aux_slice = &mut self.aux1[..];

        pool.scoped(|scope| {
            let mut current_start_index = 0;
            for pass in pass_slices {

                let aux_remain = aux_size - current_start_index;
                let cur_chunk_size = chunk_size.min(aux_remain);
                if cur_chunk_size == 0 {
                    break;
                }
                let (aux1_pass_slice, aux1_rest) = cur_aux_slice.split_at_mut(cur_chunk_size);

                scope.execute(move||{
                    for pass_slice in pass {
                        for (index,func) in pass_slice.drain(..) {
                            //dbg!(index);
                            //dbg!(current_start_index);
                            //dbg!(cur_chunk_size);
                            debug_assert!(index>=current_start_index && index<current_start_index + cur_chunk_size);
                            (func)(unsafe{aux1_pass_slice.get_unchecked_mut(index-current_start_index)});

                        }
                    }
                });
                current_start_index += cur_chunk_size;
                cur_aux_slice = aux1_rest;
            };
        });

    }

}

impl<'a,T,A1,F:for <'r> FnOnce(&'r mut A1)> MultiIterateContext<'a,T,A1,F> {
    #[inline(always)]
    fn modify_aux(&mut self, aux_index: usize, f:F) {
        let pass = aux_index / self.chunk_size;
        self.mutator_closures[pass].push((aux_index, f));
    }
}

pub struct UsizeExampleItem {
    item : usize,
}



#[cfg(test)]
mod tests {
    extern crate test;
    use super::MultiIterateMut;
    use scoped_threadpool::Pool;
    use test::Bencher;

    #[test]
    fn it_works() {

        let mut data = Vec::new();
        let mut aux1 = Vec::new();
        let mut test: MultiIterateMut<usize,String,_> = MultiIterateMut {
            data: &mut data,
            aux1: &mut aux1,
            funcs: Vec::new(),
        };
        test.data.push(37);
        test.aux1.push("Före".to_string());
        test.aux1.push("Före".to_string());

        let mut pool = Pool::new(8);
        test.run(&mut pool,|data_item, test, aux1|{
            test.modify_aux(1,|arg:&mut String|{*arg = "Hej".to_string()});
        });

        println!("Result: {:?}",test.data);
        println!("Result: {:?}",test.aux1);

        return ();

    }

    struct UsizeExampleItem {
        item : usize,
    }
    #[bench]
    fn benchmark(bench:&mut Bencher) {
        let mut pool = Pool::new(8);
        let mut data = Vec::new();
        let mut aux1 = Vec::new();
        let mut test: MultiIterateMut<UsizeExampleItem,UsizeExampleItem,_> = MultiIterateMut {
            data: &mut data,
            aux1: &mut aux1,
            funcs: Vec::new(),
        };
        let data_size = 10_000_000;
        for i in 0..data_size {
            test.data.push(UsizeExampleItem {
                item:i,
            });
            test.aux1.push(UsizeExampleItem {
                item:1,
            });
        }
        bench.iter(move||{


            test.run(&mut pool,|data_item, test, aux1|{
                let primary_value= data_item.item;

                //let prev_aux_value = aux1[primary_value].item;
                {
                    test.modify_aux(primary_value,
                                    #[inline(always)]
                                        move|aux1:&mut UsizeExampleItem|{
                                        aux1.item = primary_value;
                                    });
                }
            });

            /*for (i,aux) in test.aux1.iter_mut().enumerate() {
                assert_eq!(aux.item,i+1);
                aux.item=1;
            }*/




        });
    }
    #[bench]
    fn benchmark_simple_unithread(bench:&mut Bencher) {
        let mut data = Vec::new();
        let mut aux1 = Vec::new();
        let data_size = 10_000_000;
        for i in 0..data_size {
            data.push(UsizeExampleItem {
                item:i,
            });
            aux1.push(UsizeExampleItem {
                item:1,
            });
        }
        bench.iter(move||{


            for (aux1,data) in &mut aux1.iter_mut().zip(data.iter_mut()) {
                aux1.item = data.item + aux1.item;
            }
        });
    }

}
