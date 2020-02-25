
**TLDR**:

Typical rust thread pool implementations give no control over which particular thread
each task is scheduled on, possibly leading to intense cache thrashing for certain
problems.

**/TLDR**


# Low-overhead Parallelisation in Games

<sub>Author: Anders Musikka</sub>

<sub>Date: 2020-02-16</sub>

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

## Part 1, the Simple Case

https://github.com/avl/multi_iterate_mut/blob/master/Part1.md

## Part 2, Allowing Side Effects

https://github.com/avl/multi_iterate_mut/blob/master/Part2.md
