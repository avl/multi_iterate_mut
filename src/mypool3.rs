#![allow(unused)]
use crossbeam::channel::{Receiver};
use crossbeam::channel::bounded;
use std::thread::{JoinHandle};
use std::{thread, ptr};

use std::mem::transmute;
use std::sync::atomic::{AtomicUsize, Ordering};


use arrayvec::ArrayVec;
use std::cell::UnsafeCell;

use std::intrinsics::copy_nonoverlapping;
use std::marker::PhantomData;

#[cfg(test)]
use test::Bencher;
#[cfg(test)]
use crate::make_data;
use crate::PROB_SIZE;


const MAX_THREADS:usize = 8;
const SUB_BATCH: usize = 16384;
const MAX_CLOSURE_COUNT: usize = SUB_BATCH*MAX_THREADS;


pub struct AuxContext {

    cur_job1: AtomicUsize,
    cur_job2: AtomicUsize,

    aux_chunk_size: usize,
    defer_stores: [DeferStore;MAX_THREADS],
    cur_aux_chunk: usize,

    own_sync_state: AtomicUsize,
    friend_sync_state: *const AtomicUsize,

    pub cur_data_offset: usize,
}

pub struct AuxScheduler<'a, A> {
    aux_context: AuxContext,
    pd: PhantomData<&'a A>
}

impl<'a, A:Send+Sync> AuxScheduler<'a, A> {
    fn schedule<FA: Fn(&mut A) + Send + Sync +'a>(&mut self, aux_index: usize, f: FA) {
        let aux_chunk =  aux_index / self.aux_context.aux_chunk_size;
        self.aux_context.defer_stores[aux_chunk].add(aux_index,f);
    }
}


unsafe impl Send for AuxContext {

}
unsafe impl Sync for AuxContext {

}


impl AuxContext {
    fn new(chunk_index:usize) -> AuxContext {
        let mut helpers : ArrayVec<_> = ArrayVec::new();
        for _ in 0..MAX_THREADS {
            helpers.push(DeferStore::new());
        }
        AuxContext {
            cur_job1: AtomicUsize::new(0),
            cur_job2: AtomicUsize::new(0),
            aux_chunk_size: 0,
            defer_stores: helpers.into_inner().unwrap(),
            cur_aux_chunk: chunk_index,
            own_sync_state: AtomicUsize::new(0),
            friend_sync_state: ptr::null(),
            cur_data_offset:0,
        }
    }
}

#[repr(align(64))]
struct ThreadData {
    aux_context: UnsafeCell<AuxContext>,
    completion_receiver: Receiver<()>,
    thread_id: Option<JoinHandle<()>>,
}

pub struct Pool {
    threads: Vec<ThreadData>,
    first_aux_context: Box<AuxContext>,
}

#[derive(Clone,Debug)]
struct DeferStore {
    magic: Vec<usize>,
}

impl DeferStore {
    pub fn new() -> DeferStore {
        let rw = Vec::with_capacity(MAX_CLOSURE_COUNT*16);
        DeferStore {
            magic: rw,
        }
    }
    pub fn add<A:Send+Sync,FA:Fn(&mut A)>(&mut self, aux_index: usize, f:FA) {
        if std::mem::align_of::<FA>() > 8 {
            panic!("Mem align of FA was > 8");
        }
        if std::mem::size_of::<FA>()%8 != 0 {
            panic!("Size of of FA was not divisible by 8");
        }
        if std::mem::size_of::<usize>() != 8 {
            panic!("Size of usize was !=8");
        }
        let tot_size = 1usize+1+(std::mem::size_of::<FA>()+7)/8 + 2;
        if self.magic.len() + tot_size > self.magic.capacity() {
            panic!("Ran out of space for closures");
        }

        let mut write_pointer = self.magic.as_mut_ptr().wrapping_add(self.magic.len()) as *mut u8;

        unsafe { copy_nonoverlapping(&aux_index as *const usize as *const u8,
                                     write_pointer, 8) };

        write_pointer= write_pointer.wrapping_add(8);
        let size = std::mem::size_of::<FA>();
        unsafe { copy_nonoverlapping(&size as *const usize as *const u8,
                                     write_pointer, 8) };
        write_pointer = write_pointer.wrapping_add(8);

        let fa_ptr = write_pointer;
        unsafe { copy_nonoverlapping(&f as * const FA as *const usize as *const u8,
                                     write_pointer, std::mem::size_of::<FA>()) };

        write_pointer= write_pointer.wrapping_add((size+7)&!7);

        let f_ptr: *mut dyn FnMut(&mut A) =
            (fa_ptr as *mut FA) as *mut dyn FnMut(&mut A);
        let f_ptr_data : (usize,usize) = unsafe  { std::mem::transmute(f_ptr) };

        unsafe { copy_nonoverlapping(&f_ptr_data as *const (usize,usize) as *const u8,
                                     write_pointer, 16) };

        //write_pointer= write_pointer.wrapping_add(16);
        unsafe { self.magic.set_len(self.magic.len()+tot_size) };
        //println!("Scheduled. Write pointer is now {:?}",write_pointer);
        std::mem::forget(f);
    }
    pub fn process<A>(&mut self, aux:*mut A) {

        let mut read_ptr = self.magic.as_ptr() as *const u8;
        loop {
            //println!("Processing. Read_ptr = {:?}, write_pointer: {:?}",read_ptr,write_pointer);

            let write_pointer = self.magic.as_mut_ptr().wrapping_add(self.magic.len()) as *mut u8;
            if read_ptr != write_pointer as *const u8 {
                debug_assert!(read_ptr < write_pointer);

                let aux_index = unsafe  { (read_ptr as *const usize).read() } ;
                read_ptr = read_ptr.wrapping_add(8);
                let fa_size = unsafe  { (read_ptr as *const usize).read() } ;
                read_ptr = read_ptr.wrapping_add(8);
                if fa_size > 32 {
                    panic!("fa_size too big");
                }
                read_ptr = read_ptr.wrapping_add((fa_size+7)&!7);

                let f_ptr_data:(usize,usize) = unsafe  { (read_ptr as *const (usize,usize)).read() };
                let f_ptr: *mut dyn Fn(&mut A) = unsafe  { std::mem::transmute(f_ptr_data) };
                let f = unsafe{&*f_ptr};

                read_ptr = read_ptr.wrapping_add(16);

                //println!("Reconstructured a closure to call. Calling it");
                f(unsafe{&mut *aux.wrapping_add(aux_index)});
            } else {
                break;
            }
        }
        self.magic.clear();
        //write_pointer = self.magic.as_mut_ptr() as *mut u8;

    }
}

impl Pool {

    fn exit(&mut self) {
        for thread in &mut self.threads {
            /*println!("Sending quit command to thread {:?}",
                     &unsafe{&*thread.aux_context.get()}.cur_job2 as *const AtomicUsize
            );*/
            unsafe{&*thread.aux_context.get()}.cur_job2.store(1, Ordering::SeqCst);
            thread.completion_receiver.recv().unwrap();
            thread.thread_id.take().unwrap().join().unwrap();
        }
        self.threads.clear();
    }

}

impl Drop for Pool {
    fn drop(&mut self) {
        self.exit();
    }
}

impl Pool {
    #[inline]
    pub fn execute_all<'a,A:Send+Sync,T:Send+Sync,F>(&mut self, data:&'a mut [T], aux: &'a mut [A],f:F) where
        F: Fn(&'a mut [T], &mut AuxScheduler<'a,A>)+Send
    {

        let thread_count = MAX_THREADS;
        let chunk_size = (data.len() + thread_count-1) / thread_count;
        let aux_chunk_size = (aux.len() + thread_count-1) / thread_count;

        //println!("Executing with {} threads, chunk size = {}",thread_count,chunk_size);

        if (data.len()+chunk_size-1)/chunk_size > MAX_THREADS {
            panic!("Input array is wrong size");
        }

        let data_ptr = data.as_ptr() as usize;
        let data_chunks_iterator = data.chunks_mut(chunk_size);
        let mut chunk_processors : ArrayVec<_> = ArrayVec::new();
        pub fn add_chunk_processor<F:FnMut(&mut AuxContext)>(avec: &mut ArrayVec<[F;MAX_THREADS]>, f:F) {
            avec.push(f);
        }

        let aux_ptr = aux.as_mut_ptr();
        let aux_ptr_usize = aux_ptr as usize;

        //let mut debug_count = 0usize;
        for data_chunk in data_chunks_iterator {
            let fref = &f;
            add_chunk_processor(&mut chunk_processors, move|aux_context|{
                let aux_ptr = aux_ptr_usize as *mut A;

                aux_context.cur_data_offset = ((data_chunk.as_ptr() as usize) - data_ptr) / std::mem::size_of::<T>();
                let mut our_sync_counter = 0;
                aux_context.own_sync_state.store(our_sync_counter, Ordering::SeqCst);
                //println!("Chunk processor {} starting",debug_count);

                let mut sub = data_chunk.as_mut_ptr();
                let iterations = (data_chunk.len()+SUB_BATCH-1) / SUB_BATCH + MAX_THREADS;
                let mut len_remaining = data_chunk.len();
                for _ in 0..iterations {
                    let cur_len = len_remaining.min(SUB_BATCH);
                    if len_remaining!=0 {
                        //println!("Chunk processor {} running subchunk {:?}",debug_count, sub);
                        let aux_cont = unsafe { std::mem::transmute(&mut *aux_context) };
                        fref(unsafe{std::slice::from_raw_parts_mut(sub, cur_len)}, aux_cont );
                        sub = sub.wrapping_add(cur_len);
                        len_remaining -= cur_len;
                        aux_context.cur_data_offset += cur_len;
                    }

                    //println!("Chunk processor {} waiting for friend to reach {}",debug_count,our_sync_counter);
                    loop {
                        let friend = unsafe{&*aux_context.friend_sync_state};
                        /*println!("#{}: Waiting for friend to reach {}, we have reached {} (our state: {:?}, theirs: {:?})",debug_count, our_sync_counter, our_sync_counter,
                                 &aux_context.own_sync_state as *const AtomicUsize,
                                aux_context.friend_sync_state
                        );*/
                        if friend.load(Ordering::SeqCst) >= our_sync_counter {
                            break;
                        }
                    }
                    //println!("Chunk processor {} friend ready",debug_count);
                    aux_context.defer_stores[aux_context.cur_aux_chunk].process(aux_ptr);
                    our_sync_counter+=1;
                        //println!("Chunk processor signalling {}",our_sync_counter);
                    aux_context.own_sync_state.store(our_sync_counter, Ordering::SeqCst);
                    aux_context.cur_aux_chunk += 1;
                    aux_context.cur_aux_chunk%=MAX_THREADS;

                }

            });
            //debug_count+=1;
        }

        let mut cur_thread_num= 1;
        for (thread,arg) in self.threads.iter_mut().zip(chunk_processors.iter_mut().skip(1)) {
            let mut aux_context =  unsafe {&mut *thread.aux_context.get()};
            aux_context.cur_aux_chunk = cur_thread_num;
            aux_context.aux_chunk_size = aux_chunk_size;
            for store in aux_context.defer_stores.iter_mut() {
                store.magic.clear();
            }
            *aux_context.own_sync_state.get_mut()  = 0;
            let temp_f:&mut dyn FnOnce(&mut AuxContext) = arg;
            let runner_ref:(usize,usize) = unsafe { transmute ( temp_f as *mut dyn FnOnce(&mut AuxContext) ) };
            aux_context.cur_job1.store(runner_ref.0,Ordering::SeqCst);
            aux_context.cur_job2.store(runner_ref.1,Ordering::SeqCst);
            //println!("Ordering run start of worker");
            cur_thread_num  +=1;
        }

        {
            //println!("Running main thread worker component");
            self.first_aux_context.cur_aux_chunk = 0;
            self.first_aux_context.aux_chunk_size = aux_chunk_size;
            *self.first_aux_context.own_sync_state.get_mut() = 0;
            for store in self.first_aux_context.defer_stores.iter_mut() {
                store.magic.clear();
            }

            let first_f = &mut chunk_processors[0];
            (first_f)(&mut self.first_aux_context);
        }

        //println!("Almost done, waiting for workers to signal");
        for thread in &mut self.threads {
            loop {
                //println!("Outer checking status of {:?}",&unsafe{&*thread.aux_context.get()}.cur_job2 as *const AtomicUsize);
                if unsafe{&*thread.aux_context.get()}.cur_job2.load(Ordering::SeqCst)==0 {
                    break;
                }
            }
        }
        //println!("Done outer returning");

    }

    pub fn new() -> Pool {
        let thread_count = MAX_THREADS;
        let mut v = Vec::new();
        let core_ids = core_affinity::get_core_ids().unwrap();

        let mut completion_senders = Vec::new();
        for i in 0..(thread_count-1) {
            let (completion_sender,completion_receiver) = bounded(1);
            let aux_context = UnsafeCell::new(AuxContext::new(i+1));
            completion_senders.push(completion_sender);
            v.push(ThreadData {
                aux_context,
                completion_receiver,
                thread_id:None,
            });
        }

        core_affinity::set_for_current(core_ids[0]);

        for (i,(item,completion_sender)) in v.iter_mut().zip(completion_senders.into_iter()).enumerate() {
            let aux_context = item.aux_context.get() as usize;

            let core_id = core_ids[(i+1)%core_ids.len()];// i%core_ids.len()];

            let thread = thread::spawn(move || {
                core_affinity::set_for_current(core_id);
                let aux_context: *mut AuxContext = unsafe{std::mem::transmute(aux_context)};
                let cur_job2:&AtomicUsize;
                {
                    cur_job2 = &(unsafe{&*aux_context}.cur_job2);
                }

                loop {
                    let j2:usize = cur_job2.load(Ordering::Relaxed);

                    if j2==1 {
                        completion_sender.send(()).unwrap();
                        break;
                    }
                    if j2!=0 {
                        let cur_job1 = unsafe{&((*aux_context).cur_job1)};
                        let j1:usize = cur_job1.load(Ordering::Relaxed);
                        let job : (usize,usize) = (j1,j2);
                        let job:*mut dyn FnMut(&mut AuxContext) = unsafe { transmute(job) };
                        let fref:&mut dyn FnMut(&mut AuxContext) = unsafe{&mut *job};
                        fref(unsafe { &mut *aux_context });
                        let cur_job2 = unsafe{&((*aux_context).cur_job2)};
                        //println!("Actual worker for thread {} is done with task. Storing 0 to {:?}", i,&cur_job2);
                        cur_job2.store(0, Ordering::SeqCst);
                    }
                }
            });
            item.thread_id = Some(thread);
        }

        let mut p = Pool {
            first_aux_context: Box::new(AuxContext::new(0)),
            threads: v,
        };

        for i in 0..MAX_THREADS {
            let next;
            if i == 0 {
                next = &unsafe{&*p.threads[0].aux_context.get()}.own_sync_state as *const AtomicUsize;
            } else if i != MAX_THREADS-1 {
                    next = &unsafe{&*p.threads[i-1+1].aux_context.get()}.own_sync_state as *const AtomicUsize;
            } else {
                next = &p.first_aux_context.own_sync_state as *const AtomicUsize;
            }
            let cur;
            if i != 0 {
                cur = unsafe{&mut *p.threads[i-1].aux_context.get()};
            } else {
                cur = &mut p.first_aux_context;
            }
            cur.friend_sync_state = next;
        }

        p
    }
    #[inline]
    pub fn thread_count(&self) -> usize {
        self.threads.len()
    }
}


#[test]
pub fn mypool3_test1() {
    do_mypool3_test1();
}
pub fn do_mypool3_test1() {
    let mut pool = Pool::new();
    let mut data = Vec::new();
    let mut aux = Vec::<usize>::new();
    aux.push(42);
    for x in 0..MAX_THREADS {
        data.push(x);
    }

    pool.execute_all(&mut data, &mut aux,
        |items, ctx|
            {
                for item in &mut items.iter_mut() {
                    *item += 1;
                    ctx.schedule(0, |a|*a+=1);
                }
            }
    );

    assert_eq!(aux[0],50);
    for (idx,item) in data.iter().enumerate() {
        assert_eq!(idx+1, *item);
    }
}


#[bench]
pub fn benchmark_mypool3_simple(bench: &mut Bencher) {
    let mut pool = Pool::new();
    let mut data = make_data();
    let mut aux = Vec::<u64>::new();
    bench.iter(move || {
        pool.execute_all(&mut data, &mut aux, |datas, _ctx|{
           for x in datas.iter_mut() {
               *x+=1;
           }
        });
    });
}


#[derive(Default)]
pub struct ExampleItem {
    data: u64,
    big1: [u64;32],
    big2: [u64;32],
}
pub fn make_example_data() -> Vec<ExampleItem> {
    let mut data = Vec::new();
    let data_size = PROB_SIZE as u64;
    for i in 0..data_size {
        data.push(ExampleItem{
            data:i,
            ..Default::default()
        });
    }
    println!("Made data {}",data[0].data);
    data
}



#[bench]
pub fn benchmark_mypool3_aux(bench: &mut Bencher) {
    let mut pool = Pool::new();
    let mut data = make_example_data();
    let mut aux = make_example_data();

    compile_error!("Make som fuzz-tests. Consider if we can easily support multiple aux!");
    bench.iter(move || {
        pool.execute_all(&mut data, &mut aux, |datas, ctx|{
            for (idx,x) in datas.iter_mut().enumerate() {

                let aux_idx = ctx.aux_context.cur_data_offset+idx;
                let temp_ref = &x.data;
                {
                    ctx.schedule(aux_idx as usize, move|auxitem|{

                        auxitem.data += *temp_ref as u64;
                    })
                }
            }
        });
    });
}
