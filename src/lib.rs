#![feature(test)]
#![deny(warnings)]

use std::marker::PhantomData;
use scoped_threadpool::Pool;

#[repr(align(64))]
struct FuncThreadData<FA1> {
    data: Vec<Vec<(usize,FA1)>>
}

pub struct MultiIterateMut1<'a,'b,'c,T,X:Default, A1, FA1:for <'r> FnOnce(&'r mut A1)> {
    data : &'a mut Vec<T>,
    attached: &'c mut Vec<X>,
    aux : (&'b mut Vec<A1>,),
    #[allow(dead_code)]
    p1: PhantomData<FA1>,
}

pub struct MultiIterateMut2<'a,'b,'c,T,X:Default,
    A1, FA1:for <'r> FnOnce(&'r mut A1),
    A2, FA2:for <'r> FnOnce(&'r mut A2)
> {
    data : &'a mut Vec<T>,
    attached: &'c mut Vec<X>,
    aux : (&'b mut Vec<A1>,&'b mut Vec<A2>),
    #[allow(dead_code)]
    p1: (PhantomData<FA1>,PhantomData<FA2>),
}


impl<'a,'b,'c,T,X:Default,
    A1, FA1:for <'r> FnOnce(&'r mut A1),
> MultiIterateMut1<'a,'b,'c,T,X,A1,FA1> {
    pub fn new(data:&'a mut Vec<T>, attached: &'c mut Vec<X>, aux:(&'b mut Vec<A1>,)) -> MultiIterateMut1<'a,'b,'c,T,X,A1,FA1> {
        MultiIterateMut1 {
            data, aux,
            attached,
            p1:PhantomData
        }
    }
}

impl<'a,'b,'c,T,X:Default,
    A1, FA1:for <'r> FnOnce(&'r mut A1),
    A2, FA2:for <'r> FnOnce(&'r mut A2)
> MultiIterateMut2<'a,'b,'c, T,X,A1,FA1,A2,FA2> {
    pub fn new(data:&'a mut Vec<T>, attached: &'c mut Vec<X>, aux:(&'b mut Vec<A1>,&'b mut Vec<A2>)) -> MultiIterateMut2<'a,'b,'c,T,X,A1,FA1,A2,FA2> {
        MultiIterateMut2 {
            data, aux,
            attached,
            p1:(PhantomData,PhantomData)
        }
    }
}


pub struct MultiIterateContext1<'a,T, A1,FA1:for <'r> FnOnce(&'r mut A1)> {
    chunk_size:usize,
    aux_mutator_closures:(&'a mut FuncThreadData<FA1>,),
    p1:PhantomData<T>,
    p2:PhantomData<A1>
}

pub struct MultiIterateContext2<'a,T,
    A1,FA1:for <'r> FnOnce(&'r mut A1),
    A2,FA2:for <'r> FnOnce(&'r mut A2)
> {
    chunk_size:usize,
    aux_mutator_closures:(&'a mut FuncThreadData<FA1>,&'a mut FuncThreadData<FA2>),
    p1:PhantomData<T>,
    p2:PhantomData<A1>,
    p3:PhantomData<A2>,
}



macro_rules! emit_run_aux {
    ($self_aux:expr, $aux_closure:expr, $aux_data:expr, $name:tt) => {
        run_aux((&mut $self_aux.$name, $aux_closure.$name), $aux_data);
    };

    ($self_aux:expr, $aux_closure:expr, $aux_data:expr, $first:tt, $($next:tt),+) => {
        emit_run_aux!{ $self_aux, $aux_closure, $aux_data, $first }
        emit_run_aux!{ $self_aux, $aux_closure, $aux_data, $($next),+ }
    };
}


macro_rules! emit_init_pools {
    ($pool_size:expr, $name:tt) => {
        ((0..$pool_size).map(|_| FuncThreadData { data: (0..$pool_size).map(|_| Vec::new()).collect() }).collect::<Vec::<FuncThreadData<_>>>(),)
    };
    ($pool_size:expr, $name1:tt, $name2:tt) => {
        (
            (0..$pool_size).map(|_| FuncThreadData { data: (0..$pool_size).map(|_| Vec::new()).collect() }).collect::<Vec::<FuncThreadData<_>>>(),
            (0..$pool_size).map(|_| FuncThreadData { data: (0..$pool_size).map(|_| Vec::new()).collect() }).collect::<Vec::<FuncThreadData<_>>>()
        )
    };
}

macro_rules! emit_aux_iter {
    ($aux_closures_ref:expr, $name:tt) => {
        $aux_closures_ref.$name.iter_mut().map(|x|(x,))
    };
    ($aux_closures_ref:expr, $first:tt, $second:tt)  => {
        $aux_closures_ref.$first.iter_mut().zip($aux_closures_ref.$second.iter_mut())
    };
}


struct AuxData<'a> {
    pool: &'a mut Pool,
    pool_size: usize,
    chunk_size:usize,
}

macro_rules! make_run1 {
    ($name:ident, $fntype:tt, $aux_closures: ident, $MultiIterateContext1:ident, $aux_ref: ident, $($tuple_keys:tt),+ ) => {
        #[inline(always)]
        pub fn $name<L: $fntype + Send+Sync>(&mut self, pool: &mut Pool, l:L) {
            let pool_size = pool.thread_count() as usize;
            let chunk_size = (self.data.len() + pool_size - 1) / pool_size;

            let mut $aux_closures =
                (
                    emit_init_pools!(pool_size, $($tuple_keys),+)
                );

            {
                let selfdata = &mut self.data;

                let auxref = &self.aux;
                let $aux_ref = &mut $aux_closures;
                self.attached.resize_with(pool_size,||Default::default());
                let self_attached = &mut self.attached;
                let l_ref = &l;
                pool.scoped(move |scope| {
                    for ((datachunk_items, attached_item), aux_mutators) in &mut selfdata.chunks_mut(chunk_size)
                        .zip(self_attached.iter_mut())
                        .zip(emit_aux_iter!($aux_ref, $($tuple_keys),+)) {

                        let mut context = $MultiIterateContext1::new(
                                aux_mutators,chunk_size);

                        scope.execute(move || {
                            for item in datachunk_items {
                                l_ref(item, &mut context, auxref, attached_item);
                            }
                        });
                    }
                });
            }

            let mut auxdata = AuxData {
                pool, pool_size, chunk_size
            };

            let mut self_aux = &mut self.aux;
            emit_run_aux!(&mut self_aux, $aux_closures, &mut auxdata, $($tuple_keys),+);

        }
    }
}

impl<'a,'b,'c,T:Send,X:Send+Default,A1:Send+Sync,FA1:for <'r> FnOnce(&'r mut A1) + Send > MultiIterateMut1<'a,'b,'c,T,X,A1,FA1> {
    make_run1!(run1,
    (Fn(&mut T, &mut MultiIterateContext1<T,A1,FA1>, &(&mut Vec<A1>,), &mut X)),
    aux_closures, MultiIterateContext1, aux_closures_ref, 0);
}

impl<'a,'b,'c,T:Send,X:Send+Default,
    A1:Send+Sync,FA1:for <'r> FnOnce(&'r mut A1) + Send,
    A2:Send+Sync,FA2:for <'r> FnOnce(&'r mut A2) + Send,
> MultiIterateMut2<'a,'b,'c,T,X,A1,FA1,A2,FA2> {
    make_run1!(
        run1,
        (Fn(&mut T, &mut MultiIterateContext2<T,A1,FA1,A2,FA2>, &(&mut Vec<A1>,&mut Vec<A2>), &mut X)),
        aux_closures, MultiIterateContext2, aux_closures_ref, 0, 1);
}


#[inline]
fn run_aux<A1:Send,FA1:for <'r> FnOnce(&'r mut A1) + Send>(aux_and_closures: (&mut Vec<A1>, Vec<FuncThreadData<FA1>>) , aux_data: &mut AuxData) {
    let AuxData{pool, pool_size, chunk_size} = aux_data;
    let chunk_size = *chunk_size;
    let pool_size = *pool_size;

    let (aux1, mut aux1_closures) = aux_and_closures;
    let mut aux1_pass_slices = Vec::new();
    for _ in 0..pool_size {
        aux1_pass_slices.push(Vec::new());
    }
    for aux1_thread_output in aux1_closures.iter_mut() {

        for (pass_index,pass_data_item) in aux1_thread_output.data.iter_mut().enumerate() {
            aux1_pass_slices[pass_index].push(pass_data_item);
        }

    }
    let aux1_size = aux1.len();
    let mut cur_aux1_slice = &mut aux1[..];

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


impl<'a,T,A1,FA1:for <'r> FnOnce(&'r mut A1)> MultiIterateContext1<'a,T,A1,FA1> {
    fn new(data: (&'a mut FuncThreadData<FA1>,), size:usize) -> MultiIterateContext1<'a,T,A1,FA1> {
        MultiIterateContext1 {
            aux_mutator_closures: data,
            chunk_size: size,
            p1: PhantomData,
            p2: PhantomData
        }
    }
    #[inline(always)]
    pub fn modify_aux(&mut self, aux_index: usize, f:FA1) {
        let pass = aux_index / self.chunk_size;
        (self.aux_mutator_closures.0.data)[pass].push((aux_index, f));
    }
}

impl<'a,T,
        A1,FA1:for <'r> FnOnce(&'r mut A1),
        A2,FA2:for <'r> FnOnce(&'r mut A2)
> MultiIterateContext2<'a,T,A1,FA1,A2,FA2> {
    fn new(data: (&'a mut FuncThreadData<FA1>,&'a mut FuncThreadData<FA2>), size:usize) -> MultiIterateContext2<'a,T,A1,FA1,A2,FA2> {
        MultiIterateContext2 {
            aux_mutator_closures: data,
            chunk_size: size,
            p1: PhantomData,
            p2: PhantomData,
            p3: PhantomData
        }
    }
    #[inline(always)]
    pub fn modify_aux0(&mut self, aux_index: usize, f:FA1) {
        let pass = aux_index / self.chunk_size;
        (self.aux_mutator_closures.0.data)[pass].push((aux_index, f));
    }
    #[inline(always)]
    pub fn modify_aux1(&mut self, aux_index: usize, f:FA2) {
        let pass = aux_index / self.chunk_size;
        (self.aux_mutator_closures.1.data)[pass].push((aux_index, f));
    }
}





#[cfg(test)]
pub mod tests {
    pub struct UsizeExampleItem {
        item : usize,
        #[allow(dead_code)]
        payload: [u64;15],
    }
    extern crate test;
    use super::MultiIterateMut1;
    use super::MultiIterateMut2;
    use scoped_threadpool::Pool;
    use test::Bencher;
    use std::marker::PhantomData;

    #[test]
    fn it_works() {

        let mut data = Vec::new();
        let mut aux1 = Vec::new();
        let mut t = Vec::new();
        let mut test: MultiIterateMut1<usize,String,_,_> = MultiIterateMut1::new(&mut data, &mut t, (&mut aux1,));

        test.data.push(37);
        test.aux.0.push("Före".to_string());
        test.aux.0.push("Före".to_string());

        let mut pool = Pool::new(8);
        test.run1(&mut pool, |data_item, test, _aux1, _|{
            *data_item=38;
            test.modify_aux(1,|arg:&mut String|{*arg = "Hej".to_string()});
            //test.modify_aux(2,|arg:&mut String|{*arg = "Hej2".to_string()});
        });

        assert_eq!(test.data[0],38);
        assert_eq!(test.aux.0[0],"Före");
        assert_eq!(test.aux.0[1],"Hej");
        println!("Result: {:?}",test.data);
        println!("Result: {:?}",test.aux.0);

        return ();

    }


    #[test]
    fn it_works2() {

        let mut data = Vec::new();
        let mut aux1 = Vec::new();
        let mut aux2 = Vec::new();

        let mut attached = Vec::new();
        let mut test = MultiIterateMut2::new(&mut data, &mut attached,(&mut aux1, &mut aux2));

        test.data.push(37);
        test.aux.0.push("Före0".to_string());
        test.aux.0.push("Före0".to_string());
        test.aux.1.push("Före1".to_string());
        test.aux.1.push("Före1".to_string());

        let mut pool = Pool::new(8);
        test.run1(&mut pool, |data_item, test, _aux1,attached|{
            *data_item=38;
            test.modify_aux0(1,|arg:&mut String|{*arg = "Hej0".to_string()});
            test.modify_aux1(1,|arg:&mut String|{*arg = "Hej1".to_string()});
            *attached = 411;
        });

        assert_eq!(test.data[0],38);
        assert_eq!(test.attached[0],411);
        assert_eq!(test.aux.0[0],"Före0");
        assert_eq!(test.aux.0[1],"Hej0");
        assert_eq!(test.aux.1[0],"Före1");
        assert_eq!(test.aux.1[1],"Hej1");
        println!("Result: {:?}",test.data);
        println!("Result: {:?}",test.aux.0);

        return ();
    }


    #[bench]
    fn benchmark_multi(bench:&mut Bencher) {
        let mut pool = Pool::new(8);
        let mut data = Vec::new();
        let mut aux1 = Vec::new();
        let mut t = Vec::<()>::new();
        let mut test: MultiIterateMut1<UsizeExampleItem,_,UsizeExampleItem,_> = MultiIterateMut1 {
            data: &mut data,
            aux: (&mut aux1,),
            attached: &mut t,
            p1: PhantomData,
        };
        let data_size = 1_000_000;
        for i in 0..data_size {
            test.data.push(UsizeExampleItem {
                item:i,
                payload: Default::default(),
            });
            test.aux.0.push(UsizeExampleItem {
                item:1,
                payload: Default::default(),
            });
        }
        bench.iter(move||{


            test.run1(&mut pool, |data_item, test, _aux1,_|{
                let primary_value= data_item.item;

                {
                    test.modify_aux(primary_value,
                                    #[inline(always)]
                                        move|aux1:&mut UsizeExampleItem|{
                                        aux1.item = primary_value; //((primary_value as f64*0.001).cos().cos().cos()*100.0) as usize;
                                    });
                }

            });
            /*
            for (i,x) in test.aux.0.iter().enumerate() {
                assert_eq!(x.item, i);
            }*/

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
                payload: Default::default(),
            });
            aux1.push(UsizeExampleItem {
                item:1,
                payload: Default::default(),
            });
        }
        bench.iter(move||{


            for (aux1,data) in &mut aux1.iter_mut().zip(data.iter_mut()) {
                aux1.item = data.item; //((data.item as f64*0.001).cos().cos().cos()*100.0) as usize;
            }
        });
    }

}
