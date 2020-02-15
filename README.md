# Low-overhead parallelisation in games 

This blog post and git repo (https://github.com/avl/multi_iterate_mut) describes my search for a 
way to parallelize simulation of cars, intersections and roads in a computer game project. 
I won't be talking anything about the domain, but the problem is something like this:

There are three types of objects:
* Intersections
* Roads
* Cars

Intersections have red lights, and alternately admit cars from different roads into the 
intersection. When a car arrives at its destination, the car object is updated changing its
state from 'on road' to 'idle'.
 
The gist of it is that calculations are done for each intersection, and for each intersection
the calculation may yield an update to a road or a car. There are hundreds of thousands of
intersections, about the same number of roads, and a million cars. Calculations need to be
quick, so we should run as parallel as possible.

## Part 1, the simple case

Let's start by doing some operation on a Vec. For now, let's do something very simple, so that 
we can measure the overhead of scheduling calculations onto multiple threads.

In part 2 we will look into interactions between the different types of objects 
(intersections, roads and cars), but for now we'll just look at operations on one
system.

We want to apply an operation to each value in a Vec<T>. Maybe something like this (using the
excellent 'rayon'-crate:

````rust
pub fn run_rayon_scoped<'b, T: Sync + Send, F: Fn(&mut T) + Send + Sync + 'static>(data: &'b mut [T], f: F, thread_count: usize) {
    let chunk_size = (data.len() + thread_count - 1) / thread_count;

    rayon::scope(|s| {
        for datachunk_items in data.chunks_mut(chunk_size) {
            s.spawn(|_| {
                for item in datachunk_items {
                    f(item);
                }
            });
        }
    });
}

````

Let's start by simply incrementing each value in a Vec\<i64\>:

````rust
#[bench]
fn benchmark_rayon_scoped(bench: &mut Bencher) {
    let mut data = make_data();
    bench.iter(move || {
        run_rayon_scoped(&mut data, |item| {
            *item += 1;
        }, THREADS);
    });
}
````

Benchmark output:

<sub>(All benchmarks in this article are for 100,000 64-bit integers run on a 12 core AMD Ryzen 3900X
with 3000MHz memory in dual channel configuration. Multi-threaded examples use 12 threads.)</sub>

````
test benchmark_rayon_scoped      ... bench:      30,469 ns/iter (+/- 1,871)
````


Yay! Fearless concurrency! 100,000 elements in 30,469 nano seconds is 3 64bit adds per
nanosecond. Modern computers are wonderful! 

But wait a minute!

Since the above figure is with 12 cores, this is more than 3 nanoseconds per op. At 3+ GHz. 
I.e, this means the machine is taking 10+ cycles for a 64-bit add. Something is not right.

How long does it even take if we run the same job on a single thread?

````rust
#[bench]
fn benchmark_non_threaded(bench: &mut Bencher) {
    let mut data = make_data();
    bench.iter(move || {
        for data in data.iter_mut() {
            *data += 1;
        }
    });
}
````
  
Output:

````
test benchmark_non_threaded      ... bench:      13,909 ns/iter (+/- 186)
````

Wait a minute! Something's not right here! Doing the job on a single thread completed in 13,909
nanoseconds. Three times faster than doing the same job on 12 threads!?

I'll now give the reader a minute to figure this out before I reveal what's happening.

Ready?

No, Ok, I'll give you some more time to figure it out. (I really think you're just 
stalling out of politeness - don't wanna boast about your mad optimization chops do ya?).

Well, let's get to it.

The first thing we may note is that the problem "incrementing each value in an array" is going to be
memory/cache throughput limited, not ALU limited. So you may think it's not a good 
candidate for parallelization, the thing is going to be memory bound no matter what.
But in this case, with "just" 100,000 64 bit elements, this should easily fit within
processor cache memory.

So why is it still slow?

Well, each benchmark iteration only runs through the array once. So there's
no time for the cache to warm up. And in the next iteration there's **no** **guarantee** that each
sub slice is going to be processed **by the same core**. This leads to most of the
time being spent invalidating cache and transferring cache lines between cores (an interesting
article about this is https://en.wikipedia.org/wiki/MOESI_protocol ).

Unfortunately, there's no way to tell rayon which core a particular job should be executed on.

### A thread aware threadpool

So let's build our own threadpool which does allow this (**WARNING** experimental unsafe rust ahead!).

I'm not going to reproduce the entire code here, but the main idea is to start N threads, and
keep a crossbeam 'channel' open to each thread, sending pointers to Fn-closures to each thread.

Full code can be found here: https://github.com/avl/multi_iterate_mut/blob/master/src/mypool.rs

The code implements a thread pool with the seriously-lacking-in-imagination-name "MyPool".

The allows the user to supply a thread-number, when scheduling
execution of an operation, like this:

````rust
 pub fn execute<F:FnOnce()>(&mut self, thread_index:usize, f:F) {
        self.threads[thread_index].execute(f);
    }
````

Running the same benchmark as before but with this new thread pool:

````rust
 pub fn run_mypool<'b,T: Send+Sync, F: Fn(&mut T) + Send + Sync + 'static>(data:&'b mut [T], pool: &mut mypool::Pool, f: F) {
     let thread_count = pool.thread_count() as usize;
     let chunk_size = (data.len() + thread_count - 1) / thread_count;
     let fref = &f;
 
     pool.scoped(move |scope| {
         for (thread_index, datachunk_items) in data.chunks_mut(chunk_size).enumerate() {
             scope.execute(thread_index/*<- SPECIFIES THREAD NUMBER HERE*/, move || {
                 for item in datachunk_items {
                     fref(item);
                 }
             });
         }
     });
 }
````

Output:

````
test benchmark_mypool            ... bench:      18,165 ns/iter (+/- 5,136)
````

Well, it seems to have helped. Execution time is reduced by about 40%.
But this multi-threaded solution is slower than the single-threaded version. And
burns something like 16 times the resources. 

Clearly we're not yet done.

### A faster threadpool

So why is it still slow? 

There are a few things which aren't completely satisfactory with "MyPool":

1. It wraps each closure in a Box<dyn FnOnce>, which means heap-allocation for each closure.
2. It uses crossbeam channels, which might incur operating system level context switches 
(not crossbeam's fault, this is the only sane way to write a general channel-implementation).
3. The way scoped threads work, with two layers of closures, means that there is some overhead,
including some bounds checks in MyPool.

Let's try and solve all these problems!

The first problem is a consequence of the design that also yields the third problem.

This is how scoped threads (and "MyPool") are used:

````rust
pool.scoped(move |scope| {
    for (thread_index, datachunk_items) in data.chunks_mut(chunk_size).enumerate() {
        scope.execute(thread_index, move || {
            for item in datachunk_items {
                fref(item);
            }
        });
    }
});

````


Each call to scope.execute needs to insert a closure into the 'scope'.  
This means that the scope::execute must somehow box the closure, in order
to be able to store it. (It is actually possible to make scope generic in the type of
the closure, avoiding the need for a box, but this would have the surprising effect that 
execute could only be called with one particular closure type in each scope).

Instead, we change the interface to the threadpool and get rid of the first user
supplied closure. Instead, the pool takes an array of closures to execute.

The second "problem" can be solved by (braze yourself!) busy-waiting in the 
worker threads on an atomic flag.

A slightly monstrous (in a bad way, not a Pixar-movie way) implementation with the above two ideas can be found here:

https://github.com/avl/multi_iterate_mut/blob/master/src/mypool2.rs

It has the super-boring name MyPool2.

So, how does it perform?

Let's run the exact same benchmark as before. Output:

````
test benchmark_mypool2           ... bench:       3,680 ns/iter (+/- 132)
````

Yay, 3 times faster than the single-thread version! When run on 12 threads.

But can we do better?

### Thread affinity

The astute reader may have noticed that I have been talking about threads and cores
as if they were interchangeable terms. In reality, of course, there's no guarantee
that a particular thread will always be scheduled (by the OS) on the same CPU core.

And if this happens, we'll get back the cache-thrashing that was one of the problems
the initial rayon-based solution wasn't as fast as we liked.

Enter the core_affinity-crate!

This lets us get a list of the available cores on our machine:

````rust
let core_ids = core_affinity::get_core_ids().unwrap();
````

And then assign a particular worker thread to a specific core, using something like:

````rust
core_affinity::set_for_current(core_ids[thread_number]);
````

Let's run the benchmark again, with proper core affinity specified:

````
test benchmark_mypool2           ... bench:       2,510 ns/iter (+/- 47)
````

Yay! Even faster. About 5 times faster than the single-threaded solution.


### A broader view

We've seen that it is possible to increment each item in an array very fast if:
* Parts of it are in cache memory on different CPU-cores
* The work is performed by the right core on the right memory

We are 100% in micro-benchmark-optimization territory here. The gains presented in
this article are only going to be real for problems which fit in CPU cache, and are
such that multiple iterations are not interspersed with other cache-thrashing tasks. 
In a typical computer game, multiple systems will be simulated, and even if
one system might fit in the CPU cache, the nest system won't and when it is run it will
evict everything from the cache.

In my game, I am working on a fast-forward feature, and in this case operations
on road intersections are expected to be basically the only active jobs. I expect
multiple iterations to actually be able to run consecutively with hot caches. As far as I can tell
in these situations cache thrashing becomes so bad when using rayon or scoped-threadpool-rs,
that it can be better to run single threaded.

In situations where the work being parallelized is not memory-bandwidth/latency limited, or
the amount of work done by each task is greater, the overhead described in this article
isn't going to be felt. 

### Limitations and fundamental problems of MyPool2

MyPool2 as described above is an experiment, not broadly usable code. 

#### Busy waiting
One problem is that busy-waiting in user-space is potentially extremely inefficient.

Let's say I'm running a task on 12 threads, on a 12-core CPU. Now, anti-virus software
(or any other background process) is started. One of my 12 threads will now make progress
slower than before. The other 11 threads won't sleep, they'll just
spin and burn CPU-cycles doing nothing, waiting for the slow thread to catch up.

For a computer game, this might often work out ok in practice. In other situations it may
give your game a reputation of having wild unexplained performance variations on certain machines.

It becomes very important that the application does not try to run with higher parallelism than 
the number of available cores. Trying to use MyPool2 with 12 threads on a 6 core machine will
give absolutely horrifically bad performance. 

What we really would want is cooperation with the operating system. We'd like to signal
to the OS that we're one of 12 threads doing a lock-step job, and if we're in this particular
loop over here, we're not really busy and if the OS decides to suspend any of the other threads
it might just as well suspend all of the threads.  

#### Uneven work-distribution
Another major problem is that the crate as written will not distribute work evenly across threads.
If on thread finished before another, there is no provision for it to steal work from its
friends. It'll simply have to wait (burning CPU-cycles doing so!). 


#### Might be unsound
I haven't really made sure that what MyPool2 does it sound. And it may be buggy. Don't use
for anything important.

### Future work

I think there may be a place in the rust echo system for a thread pool which allows control
over which thread a task is run on. However, it is probably a niche thing.

I don't know enough about rayon to be able to tell if this functionality could be hacked into it.

I looked at scoped_threadpool, and it uses a big ol' mutex protecting a single work queue. It seems
possible to make something where each task has a hint which describes which thread it prefers,
and then use something like FUTEX_WAIT_BITSET/FUTEX_WAKE_BITSET on linux to selectively wake up
the preferred thread. I haven't looked in to the details, and haven't checked if something
similar exists on windows.

### Closing remarks

I'm very interested in comments, questions and ideas! I have probably missed something important!

Issues, pull requests are welcome.

 
 # License
 This text is CC BY-SA 4.0.
 The code in this repo i available under MIT-license:
 
````
 Copyright 2020 Anders Musikka
 
 Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:
 
 The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.
 
 THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

````
