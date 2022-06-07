In my university course, some interesting rust practice assignments were given. In addition to solving the problem, I also tried to do some exploration.

The assignments are as follows:

* Inter-thread communication

  Implement a lock-free SPSC channel (**S**ingle **P**roducer, **S**ingle **C**onsumer) using Rust's standard library,and let it pass the given tests. You can use atomic types, but not MPSC and blocking synchronization primitives ( Mutex, RwLock ... ). This limited version may have better performance than MPSC channel.

  Implemented two solutions:

  * [spsc_bounded](./spsc_bounded)

    Using a bounded ring buffer as the message queue

  * [spsc_bounded_cached](./spsc_bounded_cached)

    Producers and consumers cache the head and tail indexes of the ring buffer and its occupancy.

  Also tried to implement MPSC using Mutex:

  * [mpsc_bounded](./mpsc_bounded)

    The tail index is wrapped with a mutex

  * [mpsc_bounded_cached](./mpsc_bounded_cached)

    Each producer has a cache, and data is first sent to the cache applied from the ring buffer, and then written to the ring buffer in batches to reduce the frequency of lock usage.

* [Asynchronous communication](./groupchat)

  Implement a chat server in two ways(thread- and event-based)

* [File recovery](./recovery)

  Recover one or more deleted jpg files in the given ext2 file system.

* [Consensus](./consensus)

  Implement the raft consensus protocol for the scenario of the distributed bank branches