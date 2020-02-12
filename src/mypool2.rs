

use crossbeam::channel::{Sender,Receiver};
use crossbeam::channel::bounded;
use std::thread::{JoinHandle};
use std::thread;
use std::ops::DerefMut;
use std::mem::transmute;

#[repr(align(64))]
struct ThreadData {
    job_sender: Sender<Option<(usize,usize)>>,
    completion_receiver: Receiver<()>,
    thread_id: JoinHandle<()>,
}

pub struct Scope {
    threads: Vec<ThreadData>,
}

impl ThreadData {
    /*
    pub fn execute<'a,F:FnOnce()+'a>(&mut self, f:F) {
        if self.running {
            panic!("Already running a job!");
        }
        self.running = true;
        let tempbox:Box<dyn FnOnce()> = Box::new(f);
        let tempbox = unsafe{transmute(tempbox)};
        self.runner  = Some(tempbox);

        let runner_ref:(usize,usize) = unsafe { transmute ( (self.runner.as_mut().unwrap()).deref_mut() as *mut dyn FnOnce() ) };
        self.job_sender.send(Some(runner_ref)).unwrap();
    }*/
}
impl Scope {

    fn exit(&mut self) {
        for thread in self.threads.drain(..) {
            thread.job_sender.send(None).unwrap();
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
    pub fn execute_all<F:FnOnce()>(&mut self, args:&[F]) {

        for (thread,arg) in self.scope.threads.iter().zip(args.iter()) {
            let temp_f:&dyn FnOnce() = arg;
            let runner_ref:(usize,usize) = unsafe { transmute ( temp_f as *const dyn FnOnce() ) };
            thread.job_sender.send(Some(runner_ref)).unwrap();
        }

        for thread in &mut self.scope.threads {
            thread.completion_receiver.recv().unwrap();
        }


    }

    pub fn new(thread_count: usize) -> Pool {
        let mut v = Vec::new();
        let core_ids = core_affinity::get_core_ids().unwrap();

        for i in 0..thread_count {
            let (job_sender,job_receiver) = bounded(1);
            let (completion_sender,completion_receiver) = bounded(1);
            let core_id = core_ids[i%core_ids.len()];// i%core_ids.len()];
            let thread = thread::spawn(move || {
                core_affinity::set_for_current(core_id);
                loop {
                    match job_receiver.recv() {
                        Ok(job) => {
                            match job {
                                Some(job) =>  {
                                    let job:*mut dyn Fn() = unsafe { transmute(job) };
                                    let fref = unsafe{&mut *job};
                                    fref();
                                    completion_sender.send(()).unwrap();
                                },
                                None => {
                                    completion_sender.send(()).unwrap();
                                    return;
                                }
                            }

                        },
                        Err(err) => {
                            panic!("{:?}",err);
                        }
                    }
                }
            });
            v.push(ThreadData {
                job_sender,
                completion_receiver,
                thread_id:thread,
            });
        }

        Pool{
            scope:Scope {
                threads: v
        }}
    }
    pub fn thread_count(&self) -> usize {
        self.scope.threads.len()
    }
}


