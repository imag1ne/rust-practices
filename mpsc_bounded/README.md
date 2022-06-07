# Bounded MPSC

The tail index is wrapped with a mutex. The performance of the channel is not very good due to the use of locks.

The benchmark as shown below:

![mpsp_bounded_cached vs mpsc](lines.svg)