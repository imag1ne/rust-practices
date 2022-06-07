//! Implementation of the Raft Consensus Protocol for a banking application.

use std::{env::args, fs, io, thread};

use rand::prelude::*;
#[allow(unused_imports)]
use tracing::{debug, info, trace, trace_span, Level};

use crate::protocol::{Accept, Message, RaftNode};
use network::{daemon, Channel, NetworkNode};

pub mod network;
pub mod protocol;

/// Creates and connects a number of branch offices for the bank.
pub fn setup_offices(office_count: usize, log_path: &str) -> io::Result<Vec<Channel<Message>>> {
    let mut channels = Vec::with_capacity(office_count);

    // create the log directory if needed
    fs::create_dir_all(log_path)?;

    // create various network nodes and start them
    for address in 0..office_count {
        let node = NetworkNode::new(address, &log_path)?;
        let address = node.address;
        channels.push(node.channel());

        thread::spawn(move || {
            // configure a span to associate log-entries with this network node
            let _guard = trace_span!("NetworkNode", id = address);
            let _guard = _guard.enter();
            let raft_node = RaftNode::new(node);
            raft_node.start();
        });
    }

    // connect the network nodes in random order
    let mut rng = thread_rng();
    for src in channels.iter() {
        for dst in channels.iter().choose_multiple(&mut rng, office_count) {
            if src.address == dst.address {
                continue;
            }
            src.send(Message::Accept(Accept::new(dst.clone())));
        }
    }

    Ok(channels)
}

fn main() -> io::Result<()> {
    use tracing_subscriber::{fmt::time::ChronoLocal, FmtSubscriber};
    let log_path = args().nth(1).unwrap_or("logs".to_string());

    // initialize the tracer
    FmtSubscriber::builder()
        .with_timer(ChronoLocal::with_format("[%Mm %Ss]".to_string()))
        .with_max_level(Level::TRACE)
        .init();

    // create and connect a number of offices
    let channels = setup_offices(6, &log_path)?;
    let copy = channels.clone();

    // activate the thread responsible for the disruption of connections
    thread::spawn(move || daemon(copy, 1.0, 1.0));

    // sample script for your convenience
    script! {
        // tell the macro which collection of channels to use
        use channels;

        // customer requests start with the branch office index,
        // followed by the source account name and a list of requests
        [0] "Alice"   => open(), deposit(50);
        [1] "Bob"     => open(), deposit(150);
        sleep();
        [1] "Charlie" => open(), deposit(50);
        sleep();
        [2] "Dave"    => open(), deposit(100);
        [4] "Bob"     => transfer("Alice", 30);
        [3] "Dave"    => withdraw(20);
        sleep();
        [5] "Dave"    => transfer("Charlie", 10);
        sleep();
        [1] "Charlie"   => withdraw(60);
        sleep(5);
    }

    Ok(())
}
