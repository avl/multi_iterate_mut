

use crossbeam::channel::{Receiver};
use crossbeam::channel::bounded;
use std::thread::{JoinHandle};
use std::thread;

use std::mem::transmute;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::borrow::Borrow;

#[repr(align(64))]
struct ThreadData {
    cur_job: Arc<(AtomicUsize,AtomicUsize)>,
    completion_receiver: Receiver<()>,
    thread_id: JoinHandle<()>,
}

pub struct Scope {
    threads: Vec<ThreadData>,
}

impl Scope {

    fn exit(&mut self) {
        for thread in self.threads.drain(..) {
            thread.cur_job.1.store(1, Ordering::SeqCst);
            thread.completion_receiver.recv().unwrap();
            thread.thread_id.join().unwrap();
        }
    }

}

pub struct Pool {
    scope: Scope
}
impl Drop for Pool {
    fn drop(&mut self) {
        self.scope.exit();
    }
}

impl Pool {
    #[inline]
    pub fn execute_all<F:FnOnce()>(&mut self, args:&[F]) {

        for (thread,arg) in self.scope.threads.iter().zip(args.iter()) {
            let temp_f:&dyn FnOnce() = arg;
            let runner_ref:(usize,usize) = unsafe { transmute ( temp_f as *const dyn FnOnce() ) };

            thread.cur_job.0.store(runner_ref.0,Ordering::SeqCst);
            thread.cur_job.1.store(runner_ref.1,Ordering::SeqCst);
            //thread.job_sender.send(Some(runner_ref)).unwrap();
        }

        for thread in &mut self.scope.threads {
            loop {
                if thread.cur_job.1.load(Ordering::SeqCst)==0 {
                    break;
                }
            }
        }


    }

    pub fn new(thread_count: usize) -> Pool {
        let mut v = Vec::new();
        let core_ids = core_affinity::get_core_ids().unwrap();

        for i in 0..thread_count {
            let (completion_sender,completion_receiver) = bounded(1);
            let core_id = core_ids[i%core_ids.len()];// i%core_ids.len()];
            let cur_job = Arc::new((AtomicUsize::new(0),AtomicUsize::new(0)));


            let cur_job_clone = Arc::clone(&cur_job);

            let thread = thread::spawn(move || {
                core_affinity::set_for_current(core_id);
                let cur_job:&(AtomicUsize,AtomicUsize) = cur_job_clone.borrow();
                loop {
                    let j2:usize = cur_job.1.load(Ordering::Relaxed);

                    if j2==1 {
                        completion_sender.send(()).unwrap();
                        break;
                    }
                    if j2!=0 {
                        let j1:usize = cur_job.0.load(Ordering::Relaxed);
                        let job : (usize,usize) = (j1,j2);
                        let job:*mut dyn Fn() = unsafe { transmute(job) };
                        let fref = unsafe{&mut *job};
                        fref();
                        cur_job.1.store(0, Ordering::SeqCst);
                    }
                }
            });
            v.push(ThreadData {
                cur_job,
                completion_receiver,
                thread_id:thread,
            });
        }

        Pool{
            scope:Scope {
                threads: v
        }}
    }
    #[inline]
    pub fn thread_count(&self) -> usize {
        self.scope.threads.len()
    }
}


