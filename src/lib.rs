#![feature(test)]

use core::any::Any;
use std::marker::PhantomData;
use scoped_threadpool::Pool;

#[repr(align(8))]
struct FuncThreadData<FA1> {
    data: Vec<Vec<(usize,FA1)>>
}


struct MultiIterateMut<'a,'b,T, A1, FA1:for <'r> FnOnce(&'r mut A1)> {
    data : &'a mut Vec<T>,
    aux1 : &'b mut Vec<A1>,
    p1: PhantomData<FA1>,
}



struct MultiIterateContext1<'a,T, A1,FA1:for <'r> FnOnce(&'r mut A1)> {
    chunk_size:usize,
    aux_mutator_closures:(&'a mut Vec<Vec<(usize, FA1)>>,),
    p1:PhantomData<T>,
    p2:PhantomData<A1>
}



macro_rules! make_run1 {
    ($name:ident) => {
        #[inline(always)]
        pub fn $name<L:Fn(&mut T, &mut MultiIterateContext1<T,A1,FA1>, &Vec<A1>) + Send+Sync>(&mut self, pool: &mut Pool, l:L) {
            let pool_size = pool.thread_count() as usize;
            let chunk_size = (self.data.len() + pool_size - 1) / pool_size;

            let mut aux1_closures: Vec<_> = (0..pool_size).map(|_| FuncThreadData { data: (0..pool_size).map(|_| Vec::new()).collect() }).collect();

            {
                let selfdata = &mut self.data;

                let aux1ref = &self.aux1;
                let aux1_closures_ref = &mut aux1_closures;

                let l_ref = &l;
                pool.scoped(move |scope| {
                    for (datachunk_items, aux1_mutators) in &mut selfdata.chunks_mut(chunk_size)
                        .zip(aux1_closures_ref.iter_mut()) {
                        let mut context = MultiIterateContext1 {
                            chunk_size,
                            aux_mutator_closures: (&mut aux1_mutators.data,),
                            p1: PhantomData,
                            p2: PhantomData
                        };

                        scope.execute(move || {
                            for item in datachunk_items {
                                l_ref(item, &mut context, aux1ref);
                            }
                        });
                    }
                });
            }
            self.run_aux(aux1_closures, pool, pool_size, chunk_size);
        }
    }
}

impl<'a,'b,T:Send,A1:Send+Sync,FA1:for <'r> FnOnce(&'r mut A1) + Send > MultiIterateMut<'a,'b,T,A1,FA1> {


    make_run1!(run1);



    #[inline]
    fn run_aux(&mut self, mut aux1_closures:Vec<FuncThreadData<FA1>>, pool: &mut Pool, pool_size: usize, chunk_size: usize) {
        let mut aux1_pass_slices = Vec::new();
        for i in 0..pool_size {
            aux1_pass_slices.push(Vec::new());
        }
        for aux1_thread_output in aux1_closures.iter_mut() {

            for (pass_index,pass_data_item) in aux1_thread_output.data.iter_mut().enumerate() {
                aux1_pass_slices[pass_index].push(pass_data_item);
            }

        }
        let aux1_size = self.aux1.len();
        let mut cur_aux1_slice = &mut self.aux1[..];

        pool.scoped(|scope| {
            let mut current_start_index = 0;
            for pass in aux1_pass_slices {

                let aux_remain = aux1_size - current_start_index;
                let cur_chunk_size = chunk_size.min(aux_remain);
                if cur_chunk_size == 0 {
                    break;
                }
                let (aux1_pass_slice, aux1_rest) = cur_aux1_slice.split_at_mut(cur_chunk_size);


                scope.execute(move||{
                    for pass_slice in pass {
                        for (index,func) in pass_slice.drain(..) {
                            debug_assert!(index>=current_start_index && index<current_start_index + cur_chunk_size);
                            (func)(unsafe{aux1_pass_slice.get_unchecked_mut(index-current_start_index)});
                        }
                    }
                });
                current_start_index += cur_chunk_size;
                cur_aux1_slice = aux1_rest;
            };
        });

    }

}

impl<'a,T,A1,FA1:for <'r> FnOnce(&'r mut A1)> MultiIterateContext1<'a,T,A1,FA1> {
    #[inline(always)]
    fn modify_aux(&mut self, aux_index: usize, f:FA1) {
        let pass = aux_index / self.chunk_size;
        (self.aux_mutator_closures.0)[pass].push((aux_index, f));
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
    use std::marker::PhantomData;

    #[test]
    fn it_works() {

        let mut data = Vec::new();
        let mut aux1 = Vec::new();
        let mut test: MultiIterateMut<usize,String,_> = MultiIterateMut {
            data: &mut data,
            aux1: &mut aux1,
            p1: PhantomData,
        };
        test.data.push(37);
        test.aux1.push("Före".to_string());
        test.aux1.push("Före".to_string());

        let mut pool = Pool::new(8);
        test.run1(&mut pool, |data_item, test, aux1|{
            *data_item=38;
            test.modify_aux(1,|arg:&mut String|{*arg = "Hej".to_string()});
        });

        assert_eq!(test.data[0],38);
        assert_eq!(test.aux1[0],"Före");
        assert_eq!(test.aux1[1],"Hej");
        println!("Result: {:?}",test.data);
        println!("Result: {:?}",test.aux1);

        return ();

    }

    struct UsizeExampleItem {
        item : usize,
    }
    #[bench]
    fn benchmark_multi(bench:&mut Bencher) {
        let mut pool = Pool::new(8);
        let mut data = Vec::new();
        let mut aux1 = Vec::new();
        let mut test: MultiIterateMut<UsizeExampleItem,UsizeExampleItem,_> = MultiIterateMut {
            data: &mut data,
            aux1: &mut aux1,
            p1: PhantomData,
        };
        let data_size = 1_000_000;
        for i in 0..data_size {
            test.data.push(UsizeExampleItem {
                item:i,
            });
            test.aux1.push(UsizeExampleItem {
                item:1,
            });
        }
        bench.iter(move||{


            test.run1(&mut pool, |data_item, test, aux1|{
                let primary_value= data_item.item;

                {
                    test.modify_aux(primary_value,
                                    #[inline(always)]
                                        move|aux1:&mut UsizeExampleItem|{
                                        aux1.item = primary_value; //((primary_value as f64*0.001).cos().cos().cos()*100.0) as usize;
                                    });
                }

            });
            for (i,x) in test.aux1.iter().enumerate() {
                assert_eq!(x.item, i);
            }

        });
    }
    #[bench]
    fn benchmark_simple_unithread(bench:&mut Bencher) {
        let mut data = Vec::new();
        let mut aux1 = Vec::new();
        let data_size = 1_000_000;
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
                aux1.item = data.item;// ((data.item as f64*0.001).cos().cos().cos()*100.0) as usize;
            }
        });
    }

}
