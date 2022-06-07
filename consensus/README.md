# Consensus

The goal of this task is to implement the raft consensus protocol for the scenario of the distributed bank branches.

The problem with this scenario is that customers can simultaneously request transactions on a common set of accounts via different, spatially distributed branches. Since not every transaction is permitted, the sequence of the requested transactions needs to be validated. For instance, it is not allowed to deposit money into a non-existent account, but it is legal to open an account in one branch and deposit money into it in another branch at the same time. This kind of problem can be solved using replicated logs, for example using raft. The logs from different branches must have no conflict and the longest log must be identically replicated by the majority of branches.

### Network Simulation

In the provided simulated network, bank branches communicate directly with each other using an unreliable MPSC channel. There is no need to worry about the serialization and deserialization, because all types that implement Send trait can be used over the connection.

The network::daemon() function randomly interrupts network connections to make them unreliable and netsplits can occur. The messages can arrive unchanged in the order in which they were sent, we assume that the underlying protocol handles these network errors correctly with sequence numbers and checksums. Only the situations of net split (the network splits into several independent sub-networks) and connection failure (a branch temporarily loses the connection to the network) are to be handled by the consensus protocol.

### Implementation

As long as the details of the Raft protocol are understood, the implementation is relatively simple than other consensus protocols. Using and extending the provided simulated network and macros is very helpful for understanding raft and debugging.