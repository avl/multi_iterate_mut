
extern crate crossbeam;

use crossbeam::channel::{Sender,Receiver};
use crossbeam::channel::bounded;
use std::thread::{JoinHandle};
use std::thread;
use std::ops::DerefMut;
use std::mem::transmute;

struct ThreadData {
    job_sender: Sender<Option<(usize,usize)>>,
    completion_receiver: Receiver<()>,
    thread_id: JoinHandle<()>,
    running: bool,
    runner: Option<Box<dyn FnOnce()>>,
}

pub struct Scope {
    threads: Vec<ThreadData>,
}

impl ThreadData {
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
    }
}
impl Scope {

    pub fn execute<F:FnOnce()>(&mut self, thread_index:usize, f:F) {

        self.threads[thread_index].execute(f);

    }
    fn wait_jobs(&mut self) {
        for thread in &mut self.threads {
            if thread.running {
                thread.completion_receiver.recv().unwrap();
                thread.running=false;
            }
        }
    }
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
    pub fn new(thread_count: usize) -> Pool {
        let mut v = Vec::new();
        for _ in 0..thread_count {
            let (job_sender,job_receiver) = bounded(1);
            let (completion_sender,completion_receiver) = bounded(1);
            let thread = thread::spawn(move || {
                loop {
                    match job_receiver.recv() {
                        Ok(job) => {
                            match job {
                                Some(job) =>  {
                                    let job:*mut dyn FnMut() = unsafe { transmute(job) };
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
                running: false,
                runner: None
            });
        }

        Pool{
            scope:Scope {
                threads: v
        }}
    }
    pub fn scoped<F:FnOnce(&mut Scope)>(&mut self, f:F) {
        f(&mut self.scope);
        self.scope.wait_jobs();
    }
    pub fn thread_count(&self) -> usize {
        self.scope.threads.len()
    }
}