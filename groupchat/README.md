# Groupchat

In this task, we should implement a chat server in Rust in two ways in order to gain practical experience with the trade-offs between thread-based and event-based programming. The fundamental problem is that servers generally have to serve several communication connections at the same time, which basically requires either the use of one thread per IO connection or the use of the main thread in connection with an event loop.

The event loop-based model reduces the hardware resource consumption of the thread-based model, such as individual thread stacks and context switching. But from practical experience, implementing event-based programs fragment the control flows and reduce maintainability. Rust's asynchronous syntax somewhat compensates for this disadvantage. Since there are libraries that provide good implementations of executors, users can choose to use them according to their needs. It makes the programming experience very similar to using a thread-based model.
