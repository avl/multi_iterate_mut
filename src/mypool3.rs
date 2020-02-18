

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


const MAX_THREADS:usize = 2;
const SUB_BATCH: usize = 2048;


pub struct AuxContext {

    cur_job1: AtomicUsize,
    cur_job2: AtomicUsize,

    aux_chunk_size: usize,
    defer_stores: [DeferStore;MAX_THREADS],
    cur_aux_chunk: usize,

    own_sync_state: AtomicUsize,
    friend_sync_state: *const AtomicUsize,
}

pub struct AuxScheduler<A> {
    aux_context: AuxContext,
    pd: PhantomData<A>
}

impl<A:Send+Sync> AuxScheduler<A> {
    fn schedule<FA: Fn(&mut A) + Send + Sync>(&mut self, aux_index: usize, f: FA) {
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
        AuxContext {
            cur_job1: AtomicUsize::new(0),
            cur_job2: AtomicUsize::new(0),
            aux_chunk_size: 0,
            defer_stores: [
                DeferStore::new(),
                DeferStore::new(),
                /*
                DeferStore::new(),
                DeferStore::new(),
                DeferStore::new(),
                DeferStore::new(),
                DeferStore::new(),
                DeferStore::new(),
                DeferStore::new(),
                DeferStore::new(),
                DeferStore::new(),
                DeferStore::new(),*/
            ],
            cur_aux_chunk: chunk_index,
            own_sync_state: AtomicUsize::new(0),
            friend_sync_state: ptr::null(),
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
    first_aux_context: AuxContext,
}

struct DeferStore {
    magic: Vec<usize>,
    write_pointer: *mut u8,
}

impl DeferStore {
    pub fn new() -> DeferStore {
        let mut rw = Vec::with_capacity(2048);
        rw.resize(2048,0);
        DeferStore {
            write_pointer: rw.as_mut_ptr() as *mut u8,
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

        unsafe { copy_nonoverlapping(&aux_index as *const usize as *const u8,
                                     self.write_pointer, 8) };

        self.write_pointer= self.write_pointer.wrapping_add(8);
        let size = std::mem::size_of::<FA>();
        unsafe { copy_nonoverlapping(&size as *const usize as *const u8,
                                     self.write_pointer, 8) };
        self.write_pointer= self.write_pointer.wrapping_add(8);

        let fa_ptr = self.write_pointer;
        unsafe { copy_nonoverlapping(&f as * const FA as *const usize as *const u8,
                                     self.write_pointer, std::mem::size_of::<FA>()) };

        self.write_pointer= self.write_pointer.wrapping_add((size+7)&!7);

        let f_ptr: *mut dyn FnMut(&mut A) =
            (fa_ptr as *mut FA) as *mut dyn FnMut(&mut A);
        let f_ptr_data : (usize,usize) = unsafe  { std::mem::transmute(f_ptr) };

        unsafe { copy_nonoverlapping(&f_ptr_data as *const (usize,usize) as *const u8,
                                     self.write_pointer, 16) };

        self.write_pointer= self.write_pointer.wrapping_add(16);
        println!("Scheduled. Write pointer is now {:?}",self.write_pointer);
        std::mem::forget(f);
    }
    pub fn process<A>(&mut self, aux:*mut A) {
        let mut read_ptr = self.magic.as_ptr() as *const u8;
        loop {
            println!("Processing. Read_ptr = {:?}, write_pointer: {:?}",read_ptr,self.write_pointer);

            if read_ptr != self.write_pointer as *const u8 {
                debug_assert!(read_ptr < self.write_pointer);

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

                println!("Reconstructured a closure to call. Calling it");
                f(unsafe{&mut *aux.wrapping_add(aux_index)});
            } else {
                break;
            }
        }
        self.write_pointer = self.magic.as_mut_ptr() as *mut u8;
    }
}

impl Pool {

    fn exit(&mut self) {
        for thread in &mut self.threads {
            println!("Sending quit command to thread {:?}",
                     &unsafe{&*thread.aux_context.get()}.cur_job2 as *const AtomicUsize
            );
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
    pub fn execute_all<A:Send+Sync,T:Send+Sync,F:Fn(&mut [T], &mut AuxScheduler<A>)+Send>(&mut self, data:&mut [T], aux: &mut [A],f:F) {

        let thread_count = MAX_THREADS;
        let chunk_size = (data.len() + thread_count-1) / thread_count;
        let aux_chunk_size = (aux.len() + thread_count-1) / thread_count;

        println!("Executing with {} threads, chunk size = {}",thread_count,chunk_size);

        if chunk_size > MAX_THREADS {
            panic!("Input array is wrong size");
        }

        let data_chunks_iterator = data.chunks_mut(chunk_size);
        let mut chunk_processors : ArrayVec<_> = ArrayVec::new();
        pub fn add_chunk_processor<F:FnMut(&mut AuxContext)>(avec: &mut ArrayVec<[F;MAX_THREADS]>, f:F) {
            avec.push(f);
        }

        let aux_ptr = aux.as_mut_ptr();
        let aux_ptr_usize = aux_ptr as usize;

        let mut debug_count = 0usize;
        for data_chunk in data_chunks_iterator {
            let fref = &f;
            add_chunk_processor(&mut chunk_processors, move|aux_context|{
                let aux_ptr = aux_ptr_usize as *mut A;

                let mut our_sync_counter = 0;
                println!("Chunk processor {} starting",debug_count);

                let mut sub = data_chunk.as_mut_ptr();
                let iterations = (data_chunk.len()+SUB_BATCH-1) / SUB_BATCH + MAX_THREADS;
                let mut len_remaining = data_chunk.len();
                for _ in 0..iterations {
                    let cur_len = len_remaining.min(SUB_BATCH);
                    if len_remaining!=0 {
                        println!("Chunk processor {} running subchunk {:?}",debug_count, sub);
                        fref(unsafe{std::slice::from_raw_parts_mut(sub,cur_len)}, unsafe { std::mem::transmute(&mut *aux_context) } );
                        sub = sub.wrapping_add(cur_len);
                        len_remaining -= cur_len;
                    }

                    println!("Chunk processor {} waiting for friend to reach {}",debug_count,our_sync_counter);
                    loop {
                        let friend = unsafe{&*aux_context.friend_sync_state};
                        if friend.load(Ordering::SeqCst) >= our_sync_counter {
                            break;
                        }
                    }
                    println!("Chunk processor {} friend ready",debug_count);
                    aux_context.defer_stores[aux_context.cur_aux_chunk].process(aux_ptr);
                    our_sync_counter+=1;
                    println!("Chunk processor {} signalling {}",debug_count, our_sync_counter);
                    aux_context.own_sync_state.store(our_sync_counter, Ordering::SeqCst);
                    aux_context.cur_aux_chunk += 1;
                    aux_context.cur_aux_chunk%=MAX_THREADS;

                }
                /*
                for subchunk in data_chunk.chunks_mut(2048) {

                    println!("Chunk processor {} running subchunk",debug_count);
                    fref(subchunk, unsafe { std::mem::transmute(&mut *aux_context) } );

                    //panic!("Reduce code duplication, and test this!")
                }

                println!("Chunk processor {} done",debug_count);

                for  _ in 0..MAX_THREADS {
                    //TODO: Reduce code duplication here
                    println!("Chunk processor {} waiting for friend to reach {}",debug_count,our_sync_counter);
                    loop {
                        let friend = unsafe{&*aux_context.friend_sync_state};
                        if friend.load(Ordering::SeqCst) >= our_sync_counter {
                            break;
                        }
                    }
                    println!("Chunk processor {} friend ready",debug_count);
                    aux_context.defer_stores[aux_context.cur_aux_chunk].process(aux_ptr);
                    our_sync_counter+=1;
                    println!("Chunk processor {} signalling {}",debug_count, our_sync_counter);
                    aux_context.own_sync_state.store(our_sync_counter, Ordering::SeqCst);
                    aux_context.cur_aux_chunk += 1;
                    aux_context.cur_aux_chunk%=MAX_THREADS;

                }
                */


            });
            debug_count+=1;
        }

        let mut cur_thread_num= 1;
        for (thread,arg) in self.threads.iter_mut().zip(chunk_processors.iter_mut().skip(1)) {
            let mut aux_context =  unsafe {&mut *thread.aux_context.get()};
            aux_context.cur_aux_chunk = cur_thread_num;
            aux_context.aux_chunk_size = aux_chunk_size;
            *aux_context.own_sync_state.get_mut()  = 0;
            let temp_f:&mut dyn FnOnce(&mut AuxContext) = arg;
            let runner_ref:(usize,usize) = unsafe { transmute ( temp_f as *mut dyn FnOnce(&mut AuxContext) ) };
            aux_context.cur_job1.store(runner_ref.0,Ordering::SeqCst);
            aux_context.cur_job2.store(runner_ref.1,Ordering::SeqCst);
            println!("Ordering run start of worker");
            cur_thread_num  +=1;
        }

        {
            println!("Running main thread worker component");
            self.first_aux_context.cur_aux_chunk = 0;
            self.first_aux_context.aux_chunk_size = aux_chunk_size;
            *self.first_aux_context.own_sync_state.get_mut() = 0;

            let first_f = &mut chunk_processors[0];
            (first_f)(&mut self.first_aux_context);
        }

        println!("Almost done, waiting for workers to signal");
        for thread in &mut self.threads {
            loop {
                //println!("Outer checking status of {:?}",&unsafe{&*thread.aux_context.get()}.cur_job2 as *const AtomicUsize);
                if unsafe{&*thread.aux_context.get()}.cur_job2.load(Ordering::SeqCst)==0 {
                    break;
                }
            }
        }
        println!("Done outer returning");

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

        for (i,(item,completion_sender)) in v.iter_mut().zip(completion_senders.into_iter()).enumerate() {
            let aux_context = item.aux_context.get() as usize;

            let core_id = core_ids[i%core_ids.len()];// i%core_ids.len()];
            let thread = thread::spawn(move || {
                core_affinity::set_for_current(core_id);
                let aux_context: *mut AuxContext = unsafe{std::mem::transmute(aux_context)};
                let cur_job2:&AtomicUsize;
                {
                    cur_job2 = &(unsafe{&*aux_context}.cur_job2);
                }

                loop {
                    let j2:usize = cur_job2.load(Ordering::Relaxed);
                    //println!("Tight worker cur_job2 {:?} = {}",cur_job2 as *const AtomicUsize, j2);
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
                        println!("Actual worker for thread {} is done with task. Storing 0 to {:?}", i,&cur_job2);
                        cur_job2.store(0, Ordering::SeqCst);
                    }
                }
            });
            item.thread_id = Some(thread);

        }

        let mut p = Pool {
            first_aux_context: AuxContext::new(0),
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

    assert_eq!(aux[0],44);
    for (idx,item) in data.iter().enumerate() {
        assert_eq!(idx+1, *item);
    }


}
compile_error!("Test more")