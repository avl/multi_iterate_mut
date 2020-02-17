

use crossbeam::channel::{Receiver};
use crossbeam::channel::bounded;
use std::thread::{JoinHandle};
use std::{thread, ptr};

use std::mem::transmute;
use std::sync::atomic::{AtomicUsize, Ordering};


use arrayvec::ArrayVec;
use std::cell::UnsafeCell;

const MAX_THREADS:usize = 2;


pub struct AuxContext {

    cur_job1: AtomicUsize,
    cur_job2: AtomicUsize,

    aux_raw_data_ptr: usize,
    defer_stores: [DeferStore;MAX_THREADS],
    cur_aux_chunk: usize,




    own_sync_state: AtomicUsize,
    friend_sync_state: *const AtomicUsize,

}

unsafe impl Send for AuxContext {

}
unsafe impl Sync for AuxContext {

}


impl AuxContext {
    pub fn new(chunk_index:usize) -> AuxContext {
        AuxContext {
            aux_raw_data_ptr: 0,
            cur_job1: AtomicUsize::new(0),
            cur_job2: AtomicUsize::new(0),
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

}

impl DeferStore {
    pub fn new() -> DeferStore {
        DeferStore {
            magic: Vec::with_capacity(2048)
        }
    }
    pub fn process(&mut self) {
        // Stuff


        self.magic.clear();
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
    pub fn execute_all<A:Send+Sync,T:Send+Sync,F:Fn(&mut [T], &mut AuxContext)+Send>(&mut self, data:&mut [T], aux: &mut [A],f:F) {

        let thread_count = MAX_THREADS;
        let chunk_size = (data.len() + thread_count-1) / thread_count;

        println!("Executing with {} threads, chunk size = {}",thread_count,chunk_size);

        if chunk_size > MAX_THREADS {
            panic!("Input array is wrong size");
        }

        let data_chunks_iterator = data.chunks_mut(chunk_size);
        let mut chunk_processors : ArrayVec<_> = ArrayVec::new();
        pub fn add_chunk_processor<F:FnMut(&mut AuxContext)>(avec: &mut ArrayVec<[F;MAX_THREADS]>, f:F) {
            avec.push(f);
        }


        let mut debug_count = 0usize;
        for data_chunk in data_chunks_iterator {
            let fref = &f;
            add_chunk_processor(&mut chunk_processors, move|aux_context|{

                let mut our_sync_counter = 0;
                println!("Chunk processor {} starting",debug_count);

                for subchunk in data_chunk.chunks_mut(2048) {

                    println!("Chunk processor {} running subchunk",debug_count);
                    fref(subchunk, aux_context);

                    println!("Chunk processor {} waiting for friend to reach {}",debug_count,our_sync_counter);
                    loop {
                        let friend = unsafe{&*aux_context.friend_sync_state};
                        if friend.load(Ordering::SeqCst) >= our_sync_counter {
                            break;
                        }
                    }
                    println!("Chunk processor {} friend ready",debug_count);
                    aux_context.defer_stores[aux_context.cur_aux_chunk].process();
                    our_sync_counter+=1;
                    println!("Chunk processor {} signalling {}",debug_count, our_sync_counter);
                    aux_context.own_sync_state.store(our_sync_counter, Ordering::SeqCst);
                    aux_context.cur_aux_chunk += 1;
                }

                println!("Chunk processor {} done",debug_count);
            });
            debug_count+=1;
        }

        for (thread,arg) in self.threads.iter_mut().zip(chunk_processors.iter_mut().skip(1)) {
            let mut aux_context =  unsafe {&mut *thread.aux_context.get()};
            aux_context.aux_raw_data_ptr = aux.as_mut_ptr() as usize;
            *aux_context.own_sync_state.get_mut()  = 0;
            let temp_f:&mut dyn FnOnce(&mut AuxContext) = arg;
            let runner_ref:(usize,usize) = unsafe { transmute ( temp_f as *mut dyn FnOnce(&mut AuxContext) ) };
            aux_context.cur_job1.store(runner_ref.0,Ordering::SeqCst);
            aux_context.cur_job2.store(runner_ref.1,Ordering::SeqCst);
            println!("Ordering run start of worker");
        }

        {
            println!("Running main thread worker component");
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

    let mut pool = Pool::new();
    let mut data = Vec::new();
    let mut aux = Vec::<usize>::new();
    for x in 0..MAX_THREADS {
        data.push(x);
    }

    pool.execute_all(&mut data, &mut aux,
        |items, _ctx|
            {
                for item in &mut items.iter_mut() {
                    *item += 1;
                }
            }

    );


}
