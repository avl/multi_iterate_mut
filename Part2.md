[Back to main](https://github.com/avl/multi_iterate_mut/blob/master/README.md)

<sub>Author: Anders Musikka</sub>

<sub>Date: 2020-02-25</sub>

# Part 2, Allowing Side Effects

In [Part 1](https://github.com/avl/multi_iterate_mut/blob/master/Part1.md) we looked at quickly doing operations on all items in a vector, using many CPU cores.

In this part we're looking at making it possible to update data in some other vector while
traversing the first vector.

This is harder than it looks, since we can't just allow the parallel threads to update
the auxiliary vectors. If we do, there will be races, unless we somehow know
that different threads won't ever update the same item.

## Motivation

In my case I really need random write access to auxiliary arrays.

An example use case is code that processes road intersections. For each intersection, ordered
queues of vehicles approaching from each direction are maintained. As cars reach the intersection,
a simulation of the intersection (including red lights) periodically needs to signal cars to
proceed or stop. 

The intersections are their own data type, and all intersections are maintained in a Vec<RoadNode>.

Vehicles also have their own data type, and are maintained in a secondary Vec<Vehicle>.

When processing a RoadNode (advancing the queue, determining which cars get a red light etc),
random access to Vehicles is necessary.

## Constraints

Since I'm writing a multi player computer game, I want total determinism. This allows lock-step simulation on
multiple computers, which in turn allows multiplayer capability without having to send massive 
amounts of data across a network.

I also want safety. As an one-man indie-developer I simply don't have time for intermittent bugs.
I don't want silly mistakes to consume my limited developer time.  

##  Solution ideas

The API I want is something like this:

````rust
pub fn execute_all<'a, T: Send + Sync, A0: Send + Sync, F: for<'b> Fn(usize, &'a mut T, Context1<'a, 'b, A0>) + 'a + Send + Sync>(
    pool: &mut Pool, data: &'a mut Vec<T>, aux0: &'a mut Vec<A0>, f: F)
````

Where:

* **T** = Type of primary object being iterated over
* **A0** = Type of auxiliary object being accessed
* **F** = Closure being called for each item T in input
* **Context1** is a struct with a 'schedule' method which allows to schedule a closure mutating 
an A0 at a specified index for execution at some unspecified time.

Context1 contains a method like

````rust
pub fn schedule<F: FnOnce(&mut A0) + Send + Sync + 'b>(&mut self, index: usize, f: F);
````

In this blog post I benchmark different implementations of this API, with a simple test case
where the input data is a Vec\<u64\>, and the auxiliary data is Vec\<u64\>. For each item in the
input Vec, the corresponding item in the auxiliary Vec is accessed and incremented.

This is of course a usecase which would be better served by a simple paralllel iteration over the input,
followed by parallel iteration over the auxiliary vector. This workload is just used as a simple
benchmark for different implementations of solutions to the actual problem.



### Idea 1: Use locks

One way to provide access to each auxiliary item is to have a Mutex for each item. 

The benchmark 'benchmark_aux_locking' in the code implements this. Running this benchmark
with 100000 items on 8 threads gives:

#### Benchmark

```` 
test benchmark_aux_locking                     ... bench:     109,450 ns/iter (+/- 1,449)
````

As is evident, this is much more expensive than just iterating over the two vectors one
after the other. For fun, I implemented that as well and the speed of doing it that way is:

````
test benchmark_mypool_aux_reference            ... bench:       7,722 ns/iter (+/- 151)
````

This is more than 10 times faster! But, since it doesn't solve our problem it doesn't help us.

#### Problems with locks

Back to the locking implementation. 
There are several problems with using locks. The biggest problem is that such a solution
will never be deterministic. The whole point of locks is to serialize parallel operations which
might otherwise happen simultaneously. The order in which the locks are grabbed will not
be deterministic. 

Also, performance isn't great. It is more than a factor 10 slower than doing the two iterations
(the cheating, unusable method).

One reason for the slowdown is that locking is actually rather expensive, even in the uncontended
case.

#### Overhead of locks

If we just remove the locking, but just do totally unsafe racy raw pointer writes, we get 
something like:

````
test benchmark_aux_unsafecell_nolocking        ... bench:      16,471 ns/iter (+/- 728)
````

Note however, that this benchmark, when I ran it, varied wildly between runs. It is rarely slower
than approximately 50000 ns though.

There is a definitive cost to locking. This surprised me, since I had previously (wrongly) assumed
that uncontended locks are so fast as to have insignificant cost. It turns out, this isn't always
the case.

That said, for most business applications, the cost of an uncontended lock really is insignificant.
The locking  'benchmark benchmark_aux_locking' does acquire 100000 locks, and still finishes in just
barely above 100000 ns. So with 8 threads, this means the cost of acquiring a lock in this case must
mathematically be below about 8 nanoseconds. So, slow compared to integer arithmetic but fast compared
to something like network access.

#### Vectorisation

One other reason there is overhead compared to the benchmark_mypool_aux_reference benchmark is
the missed vectorisation. No vectorisation will be possible for the writes to the auxiliary vector, 
since the optimizer has no way to know that subsequent accesses to this vector will be sequential 
(and in general, they won't be). This is probably one of the reasons why the benchmark_aux_unsafecell_nolocking
isn't as fast as the one with two parallel iterations (benchmark_mypool_aux_reference).

The gist of all this is that locking is good, and the right choice for many applications but 
not for us.


### Idea 2

Another idea is to simply collect all work that needs to be done, and apply this in a 
deterministic fashion after the main iteration.

Ok, full disclosure: The API proposed in a previous chapter is made with this implementation in mind.

For each item in the input data Vec, a closure is called and allowed to schedule a second closure
which knows how to update an auxiliary element.

#### Algorithm

One way to implement this is to have a per-thread Vec\<"Closure"\> and accumulate all scheduled mutating actions. 
After processing the main input, the accumulated closures
would be processed. For maximum performance we want to process this second step in parallel as
well. Doing this can be done by having a data structure like

````rust
scheduleinfo = [Vec<FnOnce(&mut A0)>; THREAD_COUNT]
````

And divide the elements in Vec<A0> into different batches, one per thread. When scheduling actions
on the auxiliary Vec, the closure would be added to a different Vec in the scheduleinfo array.

A pseudo-algorithm would be something like:

````
Given 

aux = Vec<A0>
data = Vec<T>
scheduleinfo = [Vec<FnOnce(&mut A0)>; THREAD_COUNT]
data_batch_size = (len(data)+THREAD_COUNT-1)/THREAD_COUNT
aux_batch_size = (len(aux)+THREAD_COUNT-1)/THREAD_COUNT

for thread N
# Step 1
foreach t in data[N*batch_size .. (N+1)*batch_size]
  process t
  aux_index := index of item in aux which we want to mutate
  aux_bucket := aux_index / aux_batch_size
  scheduleinfo[aux_bucket].push(<closure for mutating aux item>)

Step 2
foreach f in scheduleinfo[N]
  process f

````

#### Benchmark
I've made an implementation of this, in a module with the boring name "mypool3". 

Running it on the same problem we ran the locking implementation on, we get:

````
test mypool3::benchmark_mypool3_aux_new(modified)        ... bench:     140,464 ns/iter (+/- 4,018)
````

This is not totally bad. The locking implementation is faster, but this implementation has
deterministic behaviour. But can we do better?

#### Cache effects

One thing we may note is that this algorithm is a bit wasteful with cache. It will pollute
the cache with the closures stored in the 'scheduleinfo' vectors.

One way to avoid this, is to not run the entire thing to completion before processing 
the closures. In fact, we may stop half way, process some closures, and then continue.

Implementing this, and running the same work with a 'sub batch' size of 256 gives:

````
test mypool3::benchmark_mypool3_aux_new        ... bench:      80,804 ns/iter (+/- 16,378)
````

This is a significant improvement, and this solution is now faster than the solution using
locking. Depending on the the CPU, the size of the closures, and the size of the problem in 
general, conserving cache may give a smaller or larger effect.

A disadvantage of running the algorithm on sub-batches is that if the amount of work to
be carried out varies significantly between elements in the input data, some thread may
idle while others are working.

All in all, this 'Idea 2' seems to have some merit.

## Implementation challenges and notes

Implementing 'Idea 2' from the previous chapter was not entirely trivial.

This chapter contains some observations made during implementation, both for both part 1 and part 2
of this series.

The implementation uses lots of native pointers and unsafe code. It is entirely possible
that there are bugs.

### Pointers don't implement Send and Sync

It caught me a bit by surprise, but raw pointers in Rust don't implement Send + Sync.

This means that they must be cast to 'usize' for it to be possible to send them across
a thread boundary. And doing this means losing _all_ type information, so it does require
a lot of care to be sure not to accidentally use a pointer with wrong type.

### Using the calling thread

I initially made a thread pool with 8 threads in order to process things in parallel on 8 cores.
However, the way I did this was quite wasteful, since the calling thread would just be idling while
the 8 threads did all the work. It's of course more efficient to have 7 threads in the thread pool,
and make the calling thread take part in the work.

Since I pin the workers to their own cores (i.e, set 'thread affinity') for performance, I also need 
to pin the main thread. This may not be suitable under all circumstances.

### Allowing an arbitrary number of auxiliary vectors

It is hard in Rust to make a function which takes as its parameters an arbitrary number of Vectors of different
types. It seems like it might be possible to create some sort of compile-time linked list 
with associated types and traits, but I haven't succeeded.

I want to allow the user to use an arbitrary number of auxiliary vectors. The simplest to understand (but maybe not best) way to
do this is to manually create implementations for 1, 2, 3 .. etc auxiliary vectors.

Another way would be to use "macros by example"-macros to generate many implementations from a single template.

I tried the "macros by example"-macros solution, but the fact that rust macros are hygienic made it difficult. I would have liked to be able
to generate symbols like Context1, Context2, Context3 etc. As far as I can tell, this is not actually possible? Instead,
the macro would have to take as input parameters all symbols needed. Also, there seems to be no real simple ergonomic
way to do a for-loop in macros. These limitation may well be for the best, I don't have a strong opinion on this. I can
certainly see how a very powerful macro system could be abused.

Another way would be by using proc_macros. I haven't tried this, but it seems like it might be easier (for me,
I'm sure this all depends on our previous experience with stuff!).

Yet another way would be by building something to generate source files using build scripts. I haven't looked into this
at all.




## Limitations and further work

The code may be buggy, and I'm far from certain that is is sound.

The worker threads busy wait while they don't have anything to do. This is wasteful, they should be parked.
  
Also, it's hard to shake the feeling that it should be possible to run faster. A completely single threaded
implementation has a runtime of less than 180000 nanoseconds, so the 8 threads don't buy us much. When using real
loads, instead of just super-cheap integer arithmetic, the multiple threads will help more.


 




















 
# License
This text is CC BY-SA 4.0.
The code in this repo i available under MIT-license:

````
Copyright 2020 Anders Musikka

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

````


