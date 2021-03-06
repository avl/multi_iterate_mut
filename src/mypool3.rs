#![allow(unused)]
use crossbeam::channel::Receiver;
use crossbeam::channel::bounded;
use std::thread::JoinHandle;
use std::{thread, ptr};

use std::mem::transmute;
use std::sync::atomic::{AtomicUsize, Ordering, spin_loop_hint};


use arrayvec::ArrayVec;
use std::cell::UnsafeCell;

use std::intrinsics::copy_nonoverlapping;
use std::marker::PhantomData;

#[cfg(test)]
use test::Bencher;
#[cfg(test)]
use crate::make_data;
#[cfg(test)]
use crate::PROB_SIZE;


const MAX_THREADS: usize = 8;
const SUB_BATCH: usize = 1024;
const MAX_CLOSURE_COUNT: usize = 256*SUB_BATCH;

#[repr(align(64))]
struct SyncStateInOwnCacheline(AtomicUsize);

use core_affinity::CoreId;
use crate::ptr_holder_1::{PtrHolder1, Context1};
use crate::ptr_holder_2::{PtrHolder2, Context2};
#[cfg(test)]
use std::time::Instant;


lazy_static! { // Make sure the cores are enumerated once, at startup, since the num_cpus crate used by core_affinity crate actually only returns cores for which the current thread has affinity. So after we've started running and assigning affinity, we may get a different (wrong) number of cores.
    static ref CORES: Vec<CoreId> = {
        let mut retval = Vec::new();
        let core_ids = core_affinity::get_core_ids().unwrap();
        retval.extend(core_ids);
        retval
    };
}

#[repr(align(64))]
struct AuxContext {
    cur_job1: AtomicUsize,
    cur_job2: AtomicUsize,

    cur_nominal_chunk_size: usize,
    //aux_chunk_size: usize,
    defer_stores: [DeferStore; MAX_THREADS],
    cur_aux_chunk: usize,
    ptr_holder: usize,

    own_sync_state: SyncStateInOwnCacheline,
    friend_sync_state: *const AtomicUsize,


}

#[repr(align(64))]
pub struct AuxScheduler<'a, A> {
    aux_context: AuxContext,
    pd: PhantomData<&'a A>,
}

impl<'a, AH: AuxHolder> AuxScheduler<'a, AH> {
    #[inline(always)]
    pub(crate) fn schedule<'b, FA: FnOnce(AH) + Send + Sync + 'b>(&'b mut self, channel: u32, f: FA) {
        self.aux_context.defer_stores[channel as usize].add(f);
    }
    #[inline(always)]
    pub(crate) fn get_ah(&self) -> AH {
        unsafe { (&*(self.aux_context.ptr_holder as *mut AH)).cheap_copy()}
    }
}


unsafe impl Send for AuxContext {}

unsafe impl Sync for AuxContext {}



#[repr(align(4096))]
struct InnerPool {
    threads: [UnsafeCell<AuxContext>;MAX_THREADS],
}



#[repr(align(64))]
struct DeferStore {
    magic_len:usize,
    magic: [usize;MAX_CLOSURE_COUNT],
}

impl DeferStore {
    fn new() -> DeferStore {
        DeferStore {
            magic_len:0,
            magic: [0;MAX_CLOSURE_COUNT],
        }
    }
    #[inline(always)]
    fn add<AH: AuxHolder + Send + Sync, FA: FnOnce(AH)>(&mut self, f: FA) {

        if std::mem::align_of::<FA>() > 8 {
            panic!("Mem align of FA was > 8");
        }
        if std::mem::size_of::<FA>() % 8 != 0 {
            panic!("Size of of FA was not divisible by 8");
        }
        if std::mem::size_of::<usize>() != 8 {
            panic!("Size of usize was !=8");
        }
        let tot_size = 1usize + (std::mem::size_of::<FA>() + 7) / 8 + 1;
        debug_assert!(self.magic_len + tot_size <= self.magic.len());


        let mut write_pointer = self.magic.as_mut_ptr().wrapping_add(self.magic_len);

        let size = (std::mem::size_of::<FA>() + 7) / 8;
        unsafe {
            write_pointer.write(size);
            /*copy_nonoverlapping(&size as *const usize,
                                write_pointer, 1)*/
        };
        write_pointer = write_pointer.wrapping_add(1);

        let fa_ptr = write_pointer;
        unsafe {
            copy_nonoverlapping(&f as *const FA,
                                write_pointer as *mut FA, 1)
        };

        write_pointer = write_pointer.wrapping_add(size);

        let f_ptr: *mut dyn FnOnce(AH) =
            (fa_ptr as *mut FA) as *mut dyn FnOnce(AH);
        let f_ptr_data: (usize, usize) = unsafe { std::mem::transmute(f_ptr) };

        assert_eq!(f_ptr_data.0, fa_ptr as usize);

        unsafe {
            write_pointer.write(f_ptr_data.1);
        };

        self.magic_len += tot_size;
        std::mem::forget(f);
    }
    #[inline(always)]
    fn process<AH: AuxHolder>(&mut self, aux: AH) {
        let mut read_ptr:*const usize = self.magic.as_ptr();
        loop {

            let write_pointer = self.magic.as_mut_ptr().wrapping_add(self.magic_len);
            if read_ptr != write_pointer {
                debug_assert!(read_ptr < write_pointer);

                let fa_size = unsafe { (read_ptr as *const usize).read() };
                read_ptr = read_ptr.wrapping_add(1);
                if fa_size > 64 {
                    panic!("fa_size too big: {}",fa_size);
                }
                let fa_ptr = read_ptr;
                read_ptr = read_ptr.wrapping_add(fa_size);

                let f_ptr_data: (usize, usize) = unsafe { (fa_ptr as usize, (read_ptr as *const usize).read()) };
                let f_ptr: *mut dyn Fn(AH) = unsafe { std::mem::transmute(f_ptr_data) };
                let f = unsafe { &*f_ptr };

                read_ptr = read_ptr.wrapping_add(1);

                f(unsafe{aux.cheap_copy()});
            } else {
                break;
            }
        }
        self.magic_len=0;
    }
}

impl InnerPool {
    fn exit(&mut self) {
        for item in self.threads.iter().skip(1) {
            let item = unsafe{&*item.get()};
            item.cur_job2.store(1, Ordering::SeqCst);
        }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.exit();
    }
}



pub trait AuxHolder: Send + Sync {

    /// Safety: This is only meant to be called by internal code in mypool3.
    /// It is unsafe because te AuxHolder actually smuggles a pointer to the aux-data,
    /// and this pointer isn't actually valid for static lifetime. So copying
    /// this object could make it outlive the data it points to which would be unfortunate
    /// (although not unsound in and of itself since only the actual dereference is).
    unsafe fn cheap_copy(&self) -> Self;
}



struct ThreadData2 {
    completion_receiver: Receiver<()>,
    thread_id: Option<JoinHandle<()>>,
}
pub struct Pool {
    inner: Box<InnerPool>,
    threaddata2: Vec<ThreadData2>,
}

impl Pool {
    fn exit(&mut self) {
        self.inner.exit();
        for thread in &mut self.threaddata2 {
            thread.completion_receiver.recv().unwrap();
            thread.thread_id.take().unwrap().join().unwrap();
        }
    }
    #[inline(always)]
    pub fn thread_count(&self) -> usize {
        self.inner.thread_count()
    }
    pub fn new() -> Pool {

        InnerPool::new()

    }
    #[inline(always)]
    pub fn execute_all<'a, AH: 'a + AuxHolder, T: Send + Sync, F>(&mut self, data: &'a mut [T], aux: AH, f: F) where
        F: Fn(usize, &'a mut [T], &mut AuxScheduler<'a, AH>) + Send + 'a {
        self.inner.execute_all(data,aux,f);
    }

}


impl InnerPool {


    #[inline(always)]
    pub fn execute_all<'a, AH: 'a + AuxHolder, T: Send + Sync, F>(&mut self, data: &'a mut [T], mut aux: AH, f: F) where
        F: Fn(usize, &'a mut [T], &mut AuxScheduler<'a, AH>) + Send + 'a
    {
        if data.len() == 0 {
            return;
        }

        let thread_count = MAX_THREADS;
        let chunk_size = (data.len() + thread_count - 1) / thread_count;

        if data.len() < 256 || chunk_size * (MAX_THREADS - 1) >= data.len() {
            //The ammount of work compared to the threads is such that if work is evenly divided,
            //we'll not actually use all threads.
            //Example: 8 threads, 9 pieces of work. 1 piece per thread => not enough. Two per threads => 16 units of work => only 5 threads actually have work.
            let first = unsafe{&mut *self.threads[0].get()};
            first.cur_aux_chunk = 0;
            first.ptr_holder = &mut aux as *mut AH as usize;
            *first.own_sync_state.0.get_mut() = 0;
            for store in first.defer_stores.iter_mut() {
                store.magic_len=0;
            }
            f(0, data, unsafe { std::mem::transmute(first) });

            let first = unsafe{&mut *self.threads[0].get()};
            for defer_store in &mut first.defer_stores {
                defer_store.process(unsafe{aux.cheap_copy()});
            }
            return;
        }


        if (data.len() + chunk_size - 1) / chunk_size > MAX_THREADS {
            panic!("Input array is wrong size");
        }

        let data_ptr = data.as_ptr() as usize;
        let data_chunks_iterator = data.chunks_mut(chunk_size);
        let mut chunk_processors: ArrayVec<_> = ArrayVec::new();
        fn add_chunk_processor<F: FnMut(&mut AuxContext)>(avec: &mut ArrayVec<[F; MAX_THREADS]>, f: F) {
            avec.push(f);
        }

        let aux_ptr_usize = &mut aux as *mut AH as usize;

        //let mut debug_count = 0usize;
        for data_chunk in data_chunks_iterator {
            let fref = &f;
            let auxcopy = unsafe{aux.cheap_copy()};
            add_chunk_processor(&mut chunk_processors, move |aux_context| {
                let mut cur_data_offset = ((data_chunk.as_ptr() as usize) - data_ptr) / std::mem::size_of::<T>();
                let mut our_sync_counter = 0;
                aux_context.own_sync_state.0.store(our_sync_counter, Ordering::SeqCst);
                aux_context.ptr_holder = aux_ptr_usize;

                let mut sub = data_chunk.as_mut_ptr();
                let iterations = (aux_context.cur_nominal_chunk_size + SUB_BATCH - 1) / SUB_BATCH + MAX_THREADS;
                let mut len_remaining = data_chunk.len();
                //let mut debug_wait_cycles = 0usize;
                let mut iteration = 0;

                let friend_sync_state = aux_context.friend_sync_state;
                let mut cur_aux_chunk = aux_context.cur_aux_chunk;
                while iteration < iterations {
                    let cur_len = len_remaining.min(SUB_BATCH);

                    if len_remaining != 0 {
                        let aux_cont = unsafe { std::mem::transmute(&mut *aux_context) };
                        fref(cur_data_offset, unsafe { std::slice::from_raw_parts_mut(sub, cur_len) }, aux_cont);
                        sub = sub.wrapping_add(cur_len);
                        len_remaining -= cur_len;
                        cur_data_offset += cur_len;
                    }


                    loop {
                        let friend = unsafe { &*friend_sync_state };
                        if friend.load(Ordering::SeqCst) >= our_sync_counter {
                            aux_context.defer_stores[cur_aux_chunk].process(unsafe{auxcopy.cheap_copy()});
                            our_sync_counter += 1;
                            aux_context.own_sync_state.0.store(our_sync_counter, Ordering::SeqCst);
                            cur_aux_chunk += 1;
                            cur_aux_chunk %= MAX_THREADS;
                            iteration += 1;
                            break;
                        }
                        spin_loop_hint();
                    }
                }
            });
            //debug_count+=1;
        }

        for (cur_thread_num,aux_context) in self.threads.iter_mut().enumerate() {
            let mut aux_context = unsafe { &mut *aux_context.get() };
            aux_context.cur_aux_chunk = cur_thread_num;
            aux_context.cur_nominal_chunk_size = chunk_size;
            for store in aux_context.defer_stores.iter_mut() {
                store.magic_len=0;
            }
            *aux_context.own_sync_state.0.get_mut() = 0;

        }
        for (thread, arg) in self.threads.iter_mut().skip(1).zip(chunk_processors.iter_mut().skip(1)) {
            let aux_context = unsafe { &*thread.get() };
            let temp_f: &mut dyn FnOnce(&mut AuxContext) = arg;
            let runner_ref: (usize, usize) = unsafe { transmute(temp_f as *mut dyn FnOnce(&mut AuxContext)) };
            aux_context.cur_job1.store(runner_ref.0, Ordering::SeqCst);
            aux_context.cur_job2.store(runner_ref.1, Ordering::SeqCst);

        }

        {
            let first = unsafe{&mut *self.threads[0].get()};
            let first_f = &mut chunk_processors[0];
            (first_f)(first);
        }

        for thread in self.threads.iter().skip(1) {
            let aux_context = unsafe{&*thread.get()};
            loop {
                if aux_context.cur_job2.load(Ordering::SeqCst) == 0 {
                    break;
                }
                spin_loop_hint();
            }
        }
    }


    fn new() -> Pool {
        let thread_count = MAX_THREADS;

        let mut tdata = unsafe{Box::<InnerPool>::new_zeroed().assume_init()};

        let pointer = (tdata.as_mut() as *mut InnerPool as *mut usize) as usize;
        println!("Position: {} {} {}",pointer,pointer%4096,pointer%256);
        let core_ids = CORES.clone();

        assert!(core_ids.len() >= thread_count);
        let mut completion_senders = Vec::new();

        let mut v = Vec::new();
        for i in 0..(thread_count - 1) {
            let (completion_sender, completion_receiver) = bounded(1);
            completion_senders.push(completion_sender);
            v.push(ThreadData2 {
                completion_receiver,
                thread_id: None,
            });
        }

        core_affinity::set_for_current(core_ids[0]);

        for (i, (item, completion_sender)) in v.iter_mut().zip(completion_senders.into_iter()).enumerate() {
            let aux_context = unsafe{&mut *tdata.threads[i+1].get()};

            let core_id = core_ids[(i+1) % core_ids.len()];

            let thread = thread::spawn(move || {
                core_affinity::set_for_current(core_id);
                let aux_context: *mut AuxContext = unsafe { std::mem::transmute(aux_context) };
                let cur_job2: &AtomicUsize;
                {
                    cur_job2 = &(unsafe { &*aux_context }.cur_job2);
                }

                loop {
                    let j2: usize = cur_job2.load(Ordering::Relaxed);

                    if j2 == 1 {
                        completion_sender.send(()).unwrap();
                        break;
                    }
                    if j2 != 0 {
                        let cur_job1 = unsafe { &((*aux_context).cur_job1) };
                        let j1: usize = cur_job1.load(Ordering::Relaxed);
                        let job: (usize, usize) = (j1, j2);
                        let job: *mut dyn FnMut(&mut AuxContext) = unsafe { transmute(job) };
                        let fref: &mut dyn FnMut(&mut AuxContext) = unsafe { &mut *job };
                        fref(unsafe { &mut *aux_context });
                        let cur_job2 = unsafe { &((*aux_context).cur_job2) };
                        cur_job2.store(0, Ordering::SeqCst);
                    }
                    spin_loop_hint()
                }
            });
            item.thread_id = Some(thread);
        }

        let pref = tdata.as_mut();

        for i in 0..MAX_THREADS {
            let next;
            if i == MAX_THREADS-1 {
                next = &unsafe { &*pref.threads[0].get() }.own_sync_state.0 as *const AtomicUsize;
            } else  {
                next = &unsafe { &*pref.threads[i + 1].get() }.own_sync_state.0 as *const AtomicUsize;
            }
            let cur = unsafe { &mut *pref.threads[i].get() };
            cur.friend_sync_state = next;
        }

        Pool {
            inner: tdata,
            threaddata2: v,
        }

    }
    #[inline(always)]
    pub fn thread_count(&self) -> usize {
        self.threads.len()
    }
}


#[test]
pub fn mypool3_test1() {
    do_mypool3_test1();
}

#[cfg(test)]
pub fn do_mypool3_test1() {
    let mut pool = Pool::new();
    let mut data = Vec::new();
    let mut aux = Vec::<usize>::new();
    aux.push(42);
    for x in 0..MAX_THREADS {
        data.push(x);
    }


    pool.execute_all(&mut data, PtrHolder1::new(&mut aux, pool.thread_count()),
                     |_cur_data_offset, items, ctx|
                         {
                             for item in &mut items.iter_mut() {
                                 *item += 1;
                                 ctx.schedule(0, |aux| *aux.get0(0) += 1);
                             }
                         },
    );

    assert_eq!(aux[0], 50);
    for (idx, item) in data.iter().enumerate() {
        assert_eq!(idx + 1, *item);
    }
}


#[bench]
pub fn benchmark_mypool3_simple(bench: &mut Bencher) {
    let mut pool = Pool::new();
    let mut data = make_data();
    let mut aux = Vec::<u64>::new();
    bench.iter(move || {
        pool.execute_all(&mut data, PtrHolder1::new(&mut aux, pool.thread_count()), |_cur_data_offset,datas, _ctx| {
            for x in datas.iter_mut() {
                *x += 1;
            }
        });
    });
}

#[bench]
pub fn benchmark_mypool3_singular(bench: &mut Bencher) {
    let mut pool = Pool::new();
    let mut data = vec![1u64];
    let mut aux = Vec::<u64>::new();
    bench.iter(move || {
        pool.execute_all(&mut data, PtrHolder1::new(&mut aux, pool.thread_count()), |_cur_data_offset,datas, _ctx| {
            for x in datas.iter_mut() {
                *x += 1;
            }
        });
    });
}


#[cfg(test)]
#[derive(Default)]
pub struct ExampleItem {
    data: u64,
    //big1: [u64; 32],
    //big2: [u64; 32],
}

#[cfg(test)]
pub fn make_example_data() -> Vec<ExampleItem> {
    let mut data = Vec::new();
    let data_size = PROB_SIZE as u64;
    for i in 0..data_size {
        data.push(ExampleItem {
            data: i,
            ..Default::default()
        });
    }
    data
}

#[cfg(test)]
fn gen_data(rng: &mut XorRng, count: usize) -> Vec<u64> {
    let mut data = Vec::new();
    for _ in 0..count {
        data.push(rng.gen() as u64);
    }
    data
}


#[cfg(test)]
#[derive(Clone, Copy)]
struct XorRng {
    state: u32
}

#[cfg(test)]
impl XorRng {
    pub fn new(seed: u32) -> XorRng {
        XorRng {
            state: if seed == 0 { 0xffff_ffff } else { seed }
        }
    }

    pub fn mix_in(&mut self, seed:u32) {
        self.state ^= seed;
    }

    pub fn gen(&mut self) -> u32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        x
    }
}
/*
impl Rng for XorRng {
    fn next_u32(&mut self) -> u32 {
        self.gen()
    }
}
*/
#[cfg(test)]
fn gen_size(rng: &mut XorRng) -> usize {
    let t = (rng.gen() as usize) % 1000_000;
    if t > 500_000 {
        return t % 20;
    }
    if t > 250_000 {
        return t % 1000;
    }
    t % 100_000
}


#[cfg(test)]
#[derive(Clone, Copy)]
struct Mutator<'a> {
    rng: XorRng,
    xref: &'a u64,
    x: u64,
}

#[cfg(test)]
impl<'a> Mutator<'a> {
    pub fn new(rng: XorRng, xref: &'a u64, x: u64) -> Self {
        Mutator {
            rng,
            xref,
            x,
        }
    }
    pub fn mutate(&mut self, item: &mut u64) {
        *item = *item ^ self.x ^ *self.xref ^ (self.rng.gen() as u64);
    }
    pub fn aux_item(&mut self, max: usize) -> Option<usize> {
        if max == 0 {
            return None;
        }
        Some((self.rng.gen() as usize) % max)
    }
}

#[cfg(test)]
pub fn check_eq(a: &Vec<u64>, b: &Vec<u64>) {
    if a.len() != b.len() {
        panic!("Lengths differ {} vs should be {}", a.len(), b.len());
    }
    for (idx, (a, b)) in a.iter().zip(b.iter()).enumerate() {
        if *a != *b {
            panic!("Contents differ at index #{}: {} vs should be {}", idx, *a, *b);
        }
    }
}



pub fn execute_all<'a, T: Send + Sync, A0: Send + Sync, F: for<'b> Fn(usize, &'a mut T, Context1<'a, 'b, A0>) + 'a + Send + Sync>(
    pool: &mut Pool, data: &'a mut Vec<T>, aux0: &'a mut Vec<A0>, f: F) {
    pool.execute_all(data, PtrHolder1::new(aux0, pool.thread_count()), #[inline(always)] move |cur_data_offset,datas, ctx| {
        let mut idx = cur_data_offset;
        for x in datas.iter_mut() {
            let mycontext = Context1 {
                ptr_holder: ctx
            };
            f(idx, x, mycontext);
            idx += 1;
        }
    });
}

pub fn execute_all2<'a, T: Send + Sync, A0: Send + Sync, A1: Send + Sync, F: for<'b> Fn(usize, &'a mut T, Context2<'a, 'b, A0, A1>) + 'a + Send + Sync>(
    pool: &mut Pool, data: &'a mut Vec<T>, aux0: &'a mut Vec<A0>, aux1: &'a mut Vec<A1>, f: F) {
    pool.execute_all(data, PtrHolder2::new(aux0, aux1, pool.thread_count()), move |cur_data_offset, datas, ctx| {
        let mut idx = cur_data_offset;
        for x in datas.iter_mut() {
            let mycontext = Context2 {
                ptr_holder: ctx
            };
            f(idx, x, mycontext);
            idx += 1;
        }
    });
}


#[cfg(test)]
pub fn fuzz_iteration(seed: u32) {
    let mut pool = Pool::new();
    let mut rng = XorRng::new(seed);
    let data_size = gen_size(&mut rng);
    let aux_size = gen_size(&mut rng);

    let mut data = gen_data(&mut rng, data_size);
    let mut aux = gen_data(&mut rng, aux_size);

    let mut data2 = data.clone();
    let mut aux2 = aux.clone();

    //let start_time = Instant::now();
    //let aux_chunk_size = (aux_size + pool.thread_count() - 1) / pool.thread_count();
    //let auxlen = aux.len();

    //let aux_ptr = aux.as_ptr() as usize;
    //let aux_ptr2 = aux2.as_ptr() as usize;

    execute_all(&mut pool, &mut data, &mut aux, move |idx, x, mut ctx| {
        *x += 1;
        if aux_size == 0 {
            return;
        }
        let mut rng = rng;
        rng.mix_in(idx as u32);
        let mut mutator = Mutator::new(rng, x, idx as u64);

        if let Some(aux_idx) = mutator.aux_item(aux_size) {
            ctx.schedule(aux_idx, move |auxitem| {
                mutator.mutate(auxitem);
            });
            ctx.schedule(aux_idx, move |auxitem| {
                *auxitem = *auxitem ^ 1;
            })
        }
    });

    let chunk_size = (data2.len() + MAX_THREADS - 1) / MAX_THREADS;

    if chunk_size != 0 {
        let mut idx=0;

        for x in data2.iter_mut() {

            *x += 1;
            if aux_size != 0 {
                let mut rng = rng;
                rng.mix_in(idx as u32);
                let mut mutator = Mutator::new(rng, x, idx as u64);
                {
                    if let Some(aux_idx) = mutator.aux_item(aux_size) {
                        mutator.mutate(&mut aux2[aux_idx]);
                        (aux2[aux_idx]) ^= 1;
                    }
                }
            }
            idx+=1;
        }

    }
    check_eq(&data, &data2);
    check_eq(&aux, &aux2);
}

#[cfg(test)]
pub fn fuzz_iteration2(seed: u32) {
    let mut pool = Pool::new();
    let mut rng = XorRng::new(seed);
    let data_size = gen_size(&mut rng);
    let aux_size0 = gen_size(&mut rng);
    let aux_size1 = gen_size(&mut rng);

    let mut data = gen_data(&mut rng, data_size);
    let mut aux0 = gen_data(&mut rng, aux_size0);
    let mut aux1 = gen_data(&mut rng, aux_size1);

    let mut datab = data.clone();
    let mut aux0b = aux0.clone();
    let mut aux1b = aux1.clone();


    execute_all2(&mut pool, &mut data, &mut aux0, &mut aux1, move |idx, x, mut ctx| {
        let mut rng = rng;
        rng.mix_in(idx as u32);
        *x+=1;
        let mut mutator = Mutator::new(rng, x, idx as u64);

        if aux_size0 == 0 || aux_size1 == 0 {
            return;
        }
        let mut mutator2 = Mutator::new(rng, x, idx as u64);

        if let Some(aux_idx) = mutator.aux_item(aux_size0) {
            ctx.schedule0(aux_idx, move |auxitem| {
                mutator.mutate(auxitem);
            });
        }
        if let Some(aux_idx) = mutator2.aux_item(aux_size1) {
            ctx.schedule1(aux_idx, move |auxitem| {
                mutator2.mutate(auxitem);
            });
        }


    });

    let chunk_size = (datab.len() + MAX_THREADS - 1) / MAX_THREADS;

    if chunk_size != 0 {
        let mut idx=0;

        for x in datab.iter_mut() {
            *x+=1;

            if aux_size0 != 0 && aux_size1 != 0 {

                let mut rng = rng;
                rng.mix_in(idx as u32);
                let mut mutator = Mutator::new(rng, x, idx as u64);
                let mut mutator2 = Mutator::new(rng, x, idx as u64);
                {
                    if let Some(aux_idx) = mutator.aux_item(aux_size0) {
                        mutator.mutate(&mut aux0b[aux_idx]);
                    }
                    if let Some(aux_idx) = mutator2.aux_item(aux_size1) {
                        mutator2.mutate(&mut aux1b[aux_idx]);
                    }
                }

            }
            idx+=1;
        }


    }
    check_eq(&data, &datab);
    check_eq(&aux0, &aux0b);
    check_eq(&aux1, &aux1b);
}


#[test]
pub fn mypool3_fuzz0() {
    fuzz_iteration(0);
}

#[test]
pub fn mypool3_fuzz1() {
    fuzz_iteration(1);
}

#[test]
pub fn mypool3_fuzz2() {
    fuzz_iteration(2);
}
#[test]
pub fn mypool3_fuzz4() {
    fuzz_iteration(4);
}


#[test]
pub fn mypool3_fuzz2_0() {
    fuzz_iteration2(0);
}

#[test]
pub fn mypool3_fuzz_many1() {
    for x in 0..1000 {
        println!("Fuzzing {}",x);
        fuzz_iteration(x);
    }
}

#[test]
pub fn mypool3_fuzz_many2() {
    for x in 0..1000 {
        println!("Fuzzing2 {}",x);
        fuzz_iteration2(x);
    }
}

#[bench]
pub fn benchmark_mypool3_aux(bench: &mut Bencher) {
    let mut pool = Pool::new();
    let mut data = make_example_data();
    let mut aux = make_example_data();

    let aux_chunk_size = (aux.len() + pool.thread_count() - 1) / pool.thread_count();
    bench.iter(move || {
        pool.execute_all(&mut data, PtrHolder1::new(&mut aux, pool.thread_count()), move |cur_data_offset,datas, ctx| {
            for (idx, x) in datas.iter_mut().enumerate() {
                let aux_idx = cur_data_offset + idx;
                let temp_ref = &x.data;
                {
                    ctx.schedule((aux_idx as usize / aux_chunk_size) as u32, move |auxitem| {
                        let item = (auxitem).get0(aux_idx);
                        item.data += *temp_ref as u64;
                    })
                }
            }
        });
    });
}


#[bench]
pub fn benchmark_mypool3_double_aux_new(bench: &mut Bencher) {
    let mut pool = Pool::new();
    let mut data = make_example_data();
    let mut aux1 = make_example_data();
    let mut aux2 = make_example_data();

    //let aux1_chunk_size = (aux1.len() + pool.thread_count() - 1) / pool.thread_count();
    let dataref = &mut data;
    let auxref1 = &mut aux1;
    let auxref2 = &mut aux2;
    bench.iter(move || {
        execute_all2(&mut pool, dataref, auxref1, auxref2,
                     #[inline(always)]
                         |idx,_data,mut context|{

                         context.schedule0(idx,#[inline(always)]|auxitem|auxitem.data+=1);
                     });
    });
}

#[bench]
pub fn benchmark_mypool3_aux_new(bench: &mut Bencher) {
    let mut pool = Pool::new();
    let mut data = make_example_data();
    let mut aux = make_example_data();


    let auxref = &mut aux;
    let dataref = &mut data;
    let poolref = &mut pool;
    bench.iter(move || {
        execute_all(poolref, dataref, auxref,
                    #[inline(always)]
                        |idx, _x, mut ctx| {
                        ctx.schedule(idx,
                                     #[inline(always)] |auxitem| {
                                         auxitem.data += 1;
                                     });
                    });
    });
}



#[ignore]
#[test]
pub fn custombenchmark_mypool3_aux_new2() {

    let mut data = make_example_data();
    let mut aux = make_example_data();
    let mut it=0;
    for _ in 0..10 {
        let mut pool = Pool::new();

        let auxref = &mut aux;
        let dataref = &mut data;
        let poolref = &mut pool;

        let bef = Instant::now();
        let mut mintime = std::u128::MAX;
        let mut maxtime = 0;
        for _ in 0..10000
            {
                let bef2 = Instant::now();
                execute_all(poolref, dataref, auxref,
                            #[inline(always)]
                                |idx, _x, mut ctx| {
                                ctx.schedule(idx,
                                             #[inline(always)] |auxitem| {
                                                 auxitem.data += 1;
                                             });
                            });
                let aft2 = Instant::now();
                maxtime = maxtime.max((aft2-bef2).as_micros());
                mintime = mintime.min((aft2-bef2).as_micros());
                std::thread::yield_now();
            }
        let aft = Instant::now();
        println!("#{} Time: {:?} outlyer: {} {}",it, (aft-bef).as_micros()/10000,mintime, maxtime);
        println!("\n");
        it+=1;
    }

}



#[cfg(test)]
pub struct DoublePtrHolder<'a> {
    p1: usize,
    p2: usize,
    p1_chunksize: usize,
    p2_chunksize: usize,
    pd: PhantomData<&'a ()>,
}

#[cfg(test)]
impl<'a> DoublePtrHolder<'a> {
    pub fn new(aux1: &mut Vec<ExampleItem>, aux2: &mut Vec<ExampleItem>) -> DoublePtrHolder<'a> {
        DoublePtrHolder {
            p1: aux1.as_mut_ptr() as usize,
            p2: aux2.as_mut_ptr() as usize,
            p1_chunksize: (aux1.len() + MAX_THREADS - 1) / MAX_THREADS,
            p2_chunksize: (aux2.len() + MAX_THREADS - 1) / MAX_THREADS,
            pd: PhantomData,
        }
    }
    fn get1(&mut self, index: usize) -> &mut ExampleItem {
        unsafe { &mut *(self.p1 as *mut ExampleItem).wrapping_add(index) }
    }
    pub fn schedule1<F: FnOnce(&mut ExampleItem) + Sync + Send + 'a>(&self, ctx: &mut AuxScheduler<DoublePtrHolder<'a>>, index: usize, f: F)
    {
        ctx.schedule((index as usize / self.p1_chunksize) as u32, move |mut auxitem| {
            f( (auxitem).get1(index) )
        })
    }
}

#[cfg(test)]
impl<'a> AuxHolder for DoublePtrHolder<'a> {
    unsafe fn cheap_copy(&self) -> Self {
        DoublePtrHolder {
            p1: self.p1,
            p2: self.p2,
            p1_chunksize: self.p1_chunksize,
            p2_chunksize: self.p2_chunksize,
            pd: PhantomData,
        }
    }
}


#[bench]
pub fn benchmark_mypool3_double_aux(bench: &mut Bencher) {
    let mut pool = Pool::new();
    let mut data = make_example_data();
    let mut aux1 = make_example_data();
    let mut aux2 = make_example_data();


    let dataref = &mut data;
    let auxref1 = &mut aux1;
    let auxref2 = &mut aux2;
    bench.iter(move || {
        pool.execute_all(dataref, DoublePtrHolder::new(auxref1, auxref2), move |cur_data_offset,datas, ctx| {
            for (idx, _x) in datas.iter_mut().enumerate() {
                let aux_idx = cur_data_offset + idx;
                {
                    ctx.get_ah().schedule1(ctx, aux_idx, move |data| data.data += 1);
                }
            }
        });
    });
}

#[cfg(test)]
fn test_determinism_iteration(pool: &mut Pool, i:u32) {
    const LOCAL_PROB_SIZE: usize = PROB_SIZE;
    let mut rng = XorRng::new(i);
    let mut data_a = gen_data(&mut rng, LOCAL_PROB_SIZE);
    let mut aux_a:Vec<u64> = gen_data(&mut rng, LOCAL_PROB_SIZE).drain(..).map(|_x|0).collect();

    let mut data_b = data_a.clone();
    let mut aux_b = aux_a.clone();

    //println!("Running A-TEST!");

    execute_all(pool, &mut data_a, &mut aux_a,|idx,data,mut context|{
        let aux_idx = (*data as usize)% LOCAL_PROB_SIZE;
        //println!("a: Index {} maps to {}",idx,aux_idx);
        context.schedule(aux_idx,move|data|*data=idx as u64);
    });

    //println!("Running B-TEST!");

    execute_all(pool, &mut data_b, &mut aux_b,|idx,data,mut context|{
        let aux_idx = (*data as usize)% LOCAL_PROB_SIZE;
        //println!("b: Index {} maps to {}",idx,aux_idx);
        context.schedule(aux_idx,move|data|*data=idx as u64);
    });

    check_eq(&aux_a,&aux_b);
    check_eq(&data_a,&data_b);
}

#[test]
pub fn mypool3_try_test_determinism() {
    let mut pool = Pool::new();

    for i in 0..1000 {
        println!("Determinism pass {}",i);
        test_determinism_iteration(&mut pool,i);
    }



}



#[bench]
pub fn benchmark_mypool3_simple_new(bench: &mut Bencher) {
    let mut pool = Pool::new();
    let mut data = make_example_data();
    let mut aux1 = make_example_data();


    let dataref = &mut data;
    let auxref1 = &mut aux1;
    bench.iter(move || {
        execute_all(&mut pool, dataref, auxref1, |_idx,data,mut _context|{
            data.data += 1;
        });
    });
}

#[test]
pub fn nulltest() {
    println!("Size: {}",std::mem::size_of::<Pool>());

}