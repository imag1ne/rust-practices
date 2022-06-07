//! Contains the network-message-types for the consensus protocol and banking application.
use crate::network::{Channel, Connection};
use crate::NetworkNode;
use rand::distributions::{Distribution, Uniform};
use rand::rngs::ThreadRng;
use rand::thread_rng;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::iter;
use std::sync::mpsc::SendError;
use std::time::{Duration, Instant};
use tracing::{debug, info};

/// Message-type of the network protocol.
#[derive(Debug, Clone)]
pub enum Message {
    /// Accept a new network connection.
    Accept(Accept),
    Command(Command),
    AppendEntry(AppendEntry),
    AppendEntryResponse(AppendEntryResponse),
    RequestVote(RequestVote),
    Vote(Vote),
}

pub trait NodeMessage {
    fn execute(self, raft_node: &mut RaftNode);
}

#[derive(Debug, Clone)]
pub struct Accept {
    channel: Channel<Message>,
}

impl Accept {
    pub fn new(channel: Channel<Message>) -> Self {
        Self { channel }
    }
}

impl NodeMessage for Accept {
    fn execute(self, raft_node: &mut RaftNode) {
        let node_id = self.channel.address;
        let connection = raft_node.network_node.accept(self.channel);
        let node = Node::new(connection);
        raft_node.nodes.insert(node_id, node);

        debug!(node_id, "connection accepted");
    }
}

#[derive(Debug, Clone)]
pub struct AppendEntry {
    node_id: usize,
    term: usize,
    committed: usize,
    entry: Option<EntryData>,
}

impl NodeMessage for AppendEntry {
    fn execute(self, raft_node: &mut RaftNode) {
        if raft_node.term <= self.term {
            // If a node finds that its term is lower than other nodes, it will
            // update its term to the higher one and become a follower.
            raft_node.role = Role::Follower;
            raft_node.term = self.term;
            raft_node.leader = Some(self.node_id);
        } else {
            // If a node receives a request with a smaller term number value,
            // it will reject it.
            return;
        }

        if let Some(entry) = self.entry {
            debug!(?entry, "look up the term of the previous entry");
            let success = entry.prev_term == raft_node.prev_entry_term(entry.index);

            if raft_node.nodes[&self.node_id]
                .send_msg(Message::AppendEntryResponse(AppendEntryResponse {
                    node_id: raft_node.id,
                    term: self.term,
                    index: entry.index,
                    success,
                }))
                .is_ok()
                && success
            {
                // overwrite the log with the new log entry
                raft_node.truncate_log(entry.index);
                raft_node.push_log_entry(LogEntry::new(entry.term, entry.command));
            }
        } else {
            if self.committed > raft_node.committed {
                for index in raft_node.committed..std::cmp::min(self.committed, raft_node.log.len())
                {
                    raft_node.apply_log(index);
                    raft_node.committed += 1;
                }
            }
        }

        if !raft_node.pending_cmds.is_empty() {
            raft_node.send_pending_cmds_to_leader();
        }

        raft_node.timeout.reset_election_timeout();
    }
}

#[derive(Debug, Clone)]
pub struct AppendEntryResponse {
    node_id: usize,
    term: usize,
    index: usize,
    success: bool,
}

impl NodeMessage for AppendEntryResponse {
    fn execute(self, raft_node: &mut RaftNode) {
        debug!(
            self.node_id,
            self.term, self.index, self.success, "received append entry response"
        );
        if raft_node.term == self.term {
            if self.success {
                // If more than half of the nodes succeed,
                // commit all uncommitted logs till the index
                if raft_node.committed <= self.index {
                    raft_node.log_entry_success[self.index].insert(self.node_id);
                    if raft_node.log_entry_success[self.index].len()
                        > (raft_node.nodes.len() + 1) / 2
                    {
                        for idx in raft_node.committed..=self.index {
                            raft_node.apply_log(idx);
                            raft_node.committed += 1;
                        }
                    }
                }
            }

            let next_entry_idx = if self.success {
                // found the log entry, append next one
                self.index + 1
            } else {
                // didn't find the log entry, append previous one
                self.index - 1
            };

            let entry = raft_node.new_entry_data(next_entry_idx);
            let append_entry_msg = raft_node.new_append_entry_msg(entry.clone());
            let node = raft_node
                .nodes
                .get_mut(&self.node_id)
                .expect(&format!("unknown node {}", self.node_id));

            node.pending_entry = entry;
            if node.pending_entry.is_some() {
                node.send_msg(append_entry_msg).ok();
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct RequestVote {
    node_id: usize,
    term: usize,
    latest_log_info: LogInfo,
}

impl NodeMessage for RequestVote {
    fn execute(self, raft_node: &mut RaftNode) {
        if raft_node.term < self.term {
            raft_node.term = self.term;

            if raft_node.latest_log_info() <= self.latest_log_info {
                let vote = Message::Vote(Vote { term: self.term });
                if raft_node.nodes[&self.node_id].send_msg(vote).is_ok() {
                    raft_node.role = Role::Follower;
                    raft_node.leader = None;
                    raft_node.timeout.reset_election_timeout();

                    debug!(
                        self.term,
                        src_node_id = raft_node.id,
                        dst_node_id = self.node_id,
                        "vote"
                    );
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Vote {
    term: usize,
}

impl NodeMessage for Vote {
    fn execute(self, raft_node: &mut RaftNode) {
        if let Role::Candidate = raft_node.role {
            if raft_node.term == self.term {
                raft_node.votes += 1;
                if raft_node.votes > (raft_node.nodes.len() + 1) / 2 {
                    raft_node.role = Role::Leader;
                    raft_node.leader = None;

                    debug!(node_id = raft_node.id, "new leader is elected");

                    // Broadcast the latest log in the next heartbeat for synchronization with the followers.
                    let entry_data = raft_node.new_entry_data(raft_node.log.len().wrapping_sub(1));
                    for node in raft_node.nodes.values_mut() {
                        node.pending_entry = entry_data.clone();
                    }

                    raft_node.drain_pending_cmds_to_log();

                    for node in raft_node.nodes.values() {
                        let append_entry_msg =
                            raft_node.new_append_entry_msg(node.pending_entry.clone());
                        node.send_msg(append_entry_msg).ok();
                    }

                    raft_node.timeout.reset_heartbeat();
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum Command {
    /// Open an account with a unique name.
    Open { account: String },

    /// Deposit money into an account.
    Deposit { account: String, amount: usize },

    /// Withdraw money from an account.
    Withdraw { account: String, amount: usize },

    /// Transfer money between accounts.
    Transfer {
        src: String,
        dst: String,
        amount: usize,
    },
}

impl NodeMessage for Command {
    fn execute(self, raft_node: &mut RaftNode) {
        debug!(command = ?self, "received client request");

        if raft_node.role == Role::Leader {
            raft_node.push_log_entry(LogEntry::new(raft_node.term, self.clone()));

            let entry_data = raft_node.new_entry_data(raft_node.log.len() - 1);
            let append_entry_msg = raft_node.new_append_entry_msg(entry_data.clone());
            for node in raft_node.nodes.values_mut() {
                if node.pending_entry.is_none() {
                    node.pending_entry = entry_data.clone();
                    node.send_msg(append_entry_msg.clone()).ok();
                }
            }
        } else {
            // forwards to leader
            raft_node.pending_cmds.push(self);

            if raft_node.leader.is_some() {
                raft_node.send_pending_cmds_to_leader();
            }
        }
    }
}

#[derive(Debug, PartialOrd, PartialEq, Clone)]
pub struct LogInfo {
    latest_term: usize,
    len: usize,
}

#[derive(Debug, Clone)]
pub struct LogEntry<T> {
    term: usize,
    entry: T,
}

impl<T> LogEntry<T> {
    pub fn new(term: usize, entry: T) -> Self {
        Self { term, entry }
    }
}

#[derive(Debug, Clone)]
pub struct EntryData {
    index: usize,
    prev_term: usize,
    term: usize,
    command: Command,
}

#[derive(Debug, PartialEq)]
pub enum Role {
    Candidate,
    Follower,
    Leader,
}

pub struct Node<T> {
    connection: Connection<T>,
    pending_entry: Option<EntryData>,
}

impl<T> Node<T> {
    pub fn new(connection: Connection<T>) -> Self {
        Self {
            connection,
            pending_entry: None,
        }
    }

    pub fn send_msg(&self, msg: T) -> Result<(), SendError<T>> {
        Ok(self.connection.encode(msg)?)
    }
}

#[derive(Debug)]
pub struct Timeout {
    timeout: Instant,
    election: Uniform<Duration>,
    heartbeat: Duration,
    rng: ThreadRng,
}

impl Timeout {
    pub fn new() -> Self {
        Self {
            timeout: Instant::now(),
            election: Uniform::new_inclusive(
                Duration::from_millis(150),
                Duration::from_millis(300),
            ),
            heartbeat: Duration::from_millis(100),
            rng: thread_rng(),
        }
    }

    pub fn reset_election_timeout(&mut self) -> Instant {
        self.timeout = Instant::now() + self.election.sample(&mut self.rng);
        self.timeout
    }

    pub fn reset_heartbeat(&mut self) -> Instant {
        self.timeout = Instant::now() + self.heartbeat;
        self.timeout
    }
}

pub struct RaftNode {
    id: usize,
    role: Role,
    network_node: NetworkNode<Message>,
    nodes: HashMap<usize, Node<Message>>,
    leader: Option<usize>,
    term: usize,
    votes: usize,
    log: Vec<LogEntry<Command>>,
    log_entry_success: Vec<HashSet<usize>>,
    pending_cmds: Vec<Command>,
    // index of the first uncommitted log entry
    committed: usize,
    timeout: Timeout,
    book: HashMap<String, Cell<usize>>,
}

impl RaftNode {
    pub fn new(network_node: NetworkNode<Message>) -> Self {
        Self {
            id: network_node.address,
            role: Role::Follower,
            network_node,
            nodes: HashMap::new(),
            leader: None,
            term: 0,
            votes: 0,
            log: vec![],
            log_entry_success: vec![],
            pending_cmds: vec![],
            committed: 0,
            timeout: Timeout::new(),
            book: HashMap::new(),
        }
    }

    pub fn latest_log_info(&self) -> LogInfo {
        let term = self.log.last().map_or(0, |entry| entry.term);
        LogInfo {
            latest_term: term,
            len: self.log.len(),
        }
    }

    pub fn prev_entry_term(&self, index: usize) -> usize {
        self.log
            .get(index.wrapping_sub(1))
            .map_or(0, |entry| entry.term)
    }

    pub fn new_entry_data(&self, index: usize) -> Option<EntryData> {
        if index < self.log.len() {
            Some(EntryData {
                index,
                prev_term: self.prev_entry_term(index),
                term: self.log[index].term,
                command: self.log[index].entry.clone(),
            })
        } else {
            None
        }
    }

    pub fn new_append_entry_msg(&self, entry: Option<EntryData>) -> Message {
        Message::AppendEntry(AppendEntry {
            node_id: self.id,
            term: self.term,
            committed: self.committed,
            entry,
        })
    }

    pub fn drain_pending_cmds_to_log(&mut self) {
        let term = self.term;
        let cnt = self.pending_cmds.len();
        self.log.extend(
            self.pending_cmds
                .drain(..)
                .map(|cmd| LogEntry::new(term, cmd)),
        );
        self.log_entry_success
            .extend(iter::repeat(HashSet::from([self.id])).take(cnt));
    }

    pub fn send_pending_cmds_to_leader(&mut self) {
        let leader = self.leader.unwrap();
        let leader_node = &self.nodes[&leader];
        let mut cmds = Vec::with_capacity(self.pending_cmds.len());
        cmds.append(&mut self.pending_cmds);
        let mut cmds = cmds.into_iter();

        while let Some(cmd) = cmds.next() {
            if leader_node
                .connection
                .encode(Message::Command(cmd.clone()))
                .is_err()
            {
                self.pending_cmds.push(cmd);
                break;
            }
        }

        self.pending_cmds.extend(cmds);
    }

    pub fn apply_log(&mut self, index: usize) {
        let entry = &self.log[index].entry;
        self.network_node.append(entry);
        debug!(?entry, "committed log");

        match entry {
            Command::Open { account } => {
                if self.book.contains_key(account) {
                    info!("command execution failed: open an account for {:?}, account already exists", account);
                } else {
                    self.book.insert(account.to_string(), Cell::new(0));
                    info!("executed command: open an account for {:?}", account);
                }
            }
            Command::Deposit { account, amount } => {
                if let Some(balance) = self.book.get(account) {
                    balance.set(balance.get() + amount);
                    info!(amount, ?account, "executed command: deposit");
                } else {
                    info!(
                        amount,
                        ?account,
                        "command execution failed: deposit, account doesn't exist"
                    );
                }
            }
            Command::Withdraw { account, amount } => {
                if let Some(balance) = self.book.get(account) {
                    if *amount <= balance.get() {
                        balance.set(balance.get() - amount);
                        info!(amount, ?account, "executed command: withdraw");
                    } else {
                        info!(
                            amount,
                            ?account,
                            "command execution failed: withdraw, balance is not enough"
                        );
                    }
                } else {
                    info!(
                        amount,
                        ?account,
                        "command execution failed: withdraw, account doesn't exist"
                    );
                }
            }
            Command::Transfer { src, dst, amount } => {
                if let Some(src_balance) = self.book.get(src) {
                    if let Some(dst_balance) = self.book.get(dst) {
                        let src_blc = src_balance.get();
                        if *amount <= src_blc {
                            src_balance.set(src_blc - amount);
                            dst_balance.set(dst_balance.get() + amount);
                            info!(amount, ?src, ?dst, "applied log: transfer");
                        } else {
                            info!(
                                amount,
                                ?src,
                                ?dst,
                                "command execution failed: transfer, balance is not enough"
                            );
                        }
                    } else {
                        info!(
                            amount,
                            ?src,
                            ?dst,
                            "command execution failed: transfer, destination account doesn't exist"
                        );
                    }
                } else {
                    info!(
                        amount,
                        ?src,
                        ?dst,
                        "command execution failed: transfer, source account doesn't exist"
                    );
                };
            }
        }
    }

    pub fn push_log_entry(&mut self, entry: LogEntry<Command>) {
        self.log.push(entry);
        self.log_entry_success.push(HashSet::from([self.id]));
    }

    pub fn truncate_log(&mut self, len: usize) {
        self.log.truncate(len);
        self.log_entry_success.truncate(len);
    }

    pub fn start(mut self) {
        self.timeout.reset_election_timeout();

        loop {
            // receives messages before timeout
            while let Ok(message) = self.network_node.decode_timeout(self.timeout.timeout) {
                // dispatch messages
                match message {
                    Message::Accept(msg) => {
                        msg.execute(&mut self);
                    }
                    Message::Command(cmd) => {
                        cmd.execute(&mut self);
                    }
                    Message::AppendEntry(msg) => {
                        msg.execute(&mut self);
                    }
                    Message::AppendEntryResponse(msg) => {
                        msg.execute(&mut self);
                    }
                    Message::RequestVote(msg) => {
                        msg.execute(&mut self);
                    }
                    Message::Vote(msg) => {
                        msg.execute(&mut self);
                    }
                }
            }

            // no messages after timeout
            if self.role == Role::Leader {
                debug!("broadcast append entry");
                // sends heart-beats
                for node in self.nodes.values() {
                    let append_entry_msg = self.new_append_entry_msg(node.pending_entry.clone());
                    node.send_msg(append_entry_msg).ok();
                }

                self.timeout.reset_heartbeat();
            } else {
                // transits to candidate and starts a new election
                self.term += 1;
                self.leader = None;

                let log_info = self.latest_log_info();
                for node in self.nodes.values() {
                    node.send_msg(Message::RequestVote(RequestVote {
                        node_id: self.id,
                        term: self.term,
                        latest_log_info: log_info.clone(),
                    }))
                    .ok();
                }

                self.votes = 1;
                self.role = Role::Candidate;
                self.timeout.reset_election_timeout();
                debug!(term = self.term, "timeout, request vote");
            }
        }
    }
}

/// Helper macro for defining test-scenarios.
///
/// The basic idea is to write test-cases and then observe the behavior of the
/// simulated network through the tracing mechanism for debugging purposes.
///
/// The macro defines a mini-language to easily express sequences of commands
/// which are executed concurrently unless you explicitly pass time between them.
/// The script needs some collection of channels to operate over which has to be
/// provided as the first command (see next section).
/// Commands are separated by semicolons and are either requests (open an
/// account, deposit money, withdraw money and transfer money between accounts)
/// or other commands (currently only sleep).
///
/// # Examples
///
/// The following script creates two accounts (Foo and Bar) in different branch
/// offices, deposits money in the Foo-account, waits a second, transfers it to
/// bar, waits another half second and withdraws the money. The waiting periods
/// are critical, because we need to give the consensus-protocol time to confirm
/// the sequence of transactions before referring to changes made in a different
/// branch office. Within one branch office the timing is not important since
/// the commands are always delivered in sequence.
///
/// ```rust
///     let channels: Vec<Channel<_>>;
///     script! {
///         use channels;
///         [0] "Foo" => open(), deposit(10);
///         [1] "Bar" => open();
///         sleep();   // the argument defaults to 1 second
///         [0] "Foo" => transfer("Bar", 10);
///         sleep(0.5);// may also sleep for fractions of a second
///         [1] "Bar" => withdraw(10);
///     }
/// ```
#[macro_export]
macro_rules! script {
	// empty base case
	(@expand $chan_vec:ident .) => {};

	// meta-rule for customer requests
	(@expand $chan_vec:ident . [$id:expr] $acc:expr => $($cmd:ident($($arg:expr),*)),+; $($tail:tt)*) => {
		$(
			$chan_vec[$id].send(
				script! { @request $cmd($acc, $($arg),*) }
			);
		)*
		script! { @expand $chan_vec . $($tail)* }
	};

	// meta-rule for other commands
	(@expand $chan_vec:ident . $cmd:ident($($arg:expr),*); $($tail:tt)*) => {
		script! { @command $cmd($($arg),*) }
		script! { @expand $chan_vec . $($tail)* }
	};

	// customer requests
	(@request open($holder:expr,)) => {
		$crate::protocol::Message::Command($crate::protocol::Command::Open {
			account: $holder.into(),
		})
	};
	(@request deposit($holder:expr, $amount:expr)) => {
		$crate::protocol::Message::Command($crate::protocol::Command::Deposit {
			account: $holder.into(),
			amount: $amount,
		})
	};
	(@request withdraw($holder:expr, $amount:expr)) => {
		$crate::protocol::Message::Command($crate::protocol::Command::Withdraw {
			account: $holder.into(),
			amount: $amount,
		})
	};
	(@request transfer($src:expr, $dst:expr, $amount:expr)) => {
		$crate::protocol::Message::Command($crate::protocol::Command::Transfer {
			src: $src.into(),
			dst: $dst.into(),
			amount: $amount,
		})
	};

	// other commands
	(@command sleep($time:expr)) => {
		std::thread::sleep(std::time::Duration::from_millis(($time as f64 * 1000.0) as u64));
	};
	(@command sleep()) => {
		std::thread::sleep(std::time::Duration::from_millis(1000));
	};

	// entry point for the user
	(use $chan_vec:expr; $($tail:tt)*) => {
		let ref channels = $chan_vec;
		script! { @expand channels . $($tail)* }
	};

	// rudimentary error diagnostics
	(@request $cmd:ident $($tail:tt)*) => {
		compile_error!("maybe you mean one of open, deposit, withdraw or transfer?")
	};
	(@command $cmd:ident $($tail:tt)*) => {
		compile_error!("maybe you mean sleep or forgot the branch index?")
	};
	(@expand $($tail:tt)*) => {
		compile_error!("illegal command syntax")
	};
	($($tail:tt)*) => {
		compile_error!("missing initial 'use <channels>;'")
	};
}
