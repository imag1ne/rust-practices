#![allow(unused_imports)]
use chatlib::stream_agent::StreamAgent;
use chatlib::{would_block, Command, Deserialize, Serialize};
use mio::event::Event;
use mio::{
    net::{TcpListener, TcpStream},
    Events, Interest, Poll, Token,
};
use std::collections::{HashMap, VecDeque};
use std::{
    env,
    io::{self, Write},
};

#[derive(Debug)]
pub enum ServerCmd {
    Announcement(String),
    Broadcast(Token, String),
    Connect(TcpStream),
    Disconnect(Token),
}

pub struct Client {
    name: String,
    stream: StreamAgent<TcpStream>,
}

impl Client {
    pub fn new(stream: StreamAgent<TcpStream>) -> Self {
        Self {
            name: Default::default(),
            stream,
        }
    }
}

const SERVER: Token = Token(0);

fn main() -> io::Result<()> {
    let mut poll = Poll::new()?;
    let mut events = Events::with_capacity(128);
    let addr = env::args().nth(1).unwrap_or(String::from("127.0.0.1:8000"));
    let mut listener = TcpListener::bind(addr.parse().unwrap())?;

    poll.registry()
        .register(&mut listener, SERVER, Interest::READABLE)?;

    let mut clients: HashMap<Token, Client> = HashMap::new();
    let mut server_cmds_channel = VecDeque::new();
    let mut unique_token = Token(SERVER.0 + 1);

    loop {
        poll.poll(&mut events, None)?;

        for event in &events {
            match event.token() {
                SERVER => loop {
                    let (stream, addr) = match listener.accept() {
                        Ok((stream, addr)) => (stream, addr),
                        Err(err) => {
                            if would_block(&err) {
                                break;
                            }
                            eprintln!("{}", err);
                            continue;
                        }
                    };

                    println!("Accepted connection: {}", addr);

                    server_cmds_channel.push_back(ServerCmd::Connect(stream));
                },
                token => {
                    if let Some(client) = clients.get_mut(&token) {
                        handle_client_event(client, event, &mut server_cmds_channel);
                    }
                }
            }
        }

        while let Some(server_cmd) = server_cmds_channel.pop_front() {
            match server_cmd {
                ServerCmd::Announcement(msg) => {
                    for (token, client) in &mut clients {
                        if client.stream.encode(Command::Message(msg.clone())).is_err() {
                            server_cmds_channel.push_back(ServerCmd::Disconnect(*token));
                        }
                    }
                }
                ServerCmd::Broadcast(src_token, msg) => {
                    for (token, client) in &mut clients {
                        if *token != src_token {
                            if client.stream.encode(Command::Message(msg.clone())).is_err() {
                                server_cmds_channel.push_back(ServerCmd::Disconnect(*token));
                            }
                        }
                    }
                }
                ServerCmd::Connect(stream) => {
                    let mut client = Client::new(StreamAgent::new(stream));

                    let token = next_token(&mut unique_token);
                    poll.registry().register(
                        client.stream.principal_ref_mut(),
                        token,
                        Interest::READABLE | Interest::WRITABLE,
                    )?;

                    clients.insert(token, client);
                }
                ServerCmd::Disconnect(token) => {
                    if let Some(client) = clients.remove(&token) {
                        server_cmds_channel.push_back(ServerCmd::Announcement(format!(
                            "----- {} has left -----",
                            client.name
                        )));
                    }
                }
            }
        }
    }
}

fn handle_client_event(
    client: &mut Client,
    event: &Event,
    server_cmds_channel: &mut VecDeque<ServerCmd>,
) {
    if event.is_readable() {
        if client.stream.read_from_stream().is_ok() {
            while let Ok(cmd) = client.stream.decode() {
                match cmd {
                    Command::Name(name) => {
                        let old_name = client.name.clone();
                        let msg = if old_name != name {
                            client.name = name.clone();
                            if old_name.is_empty() {
                                format!("----- Welcome, {} -----", name)
                            } else {
                                format!("----- {} changed name to 👉 {} -----", old_name, name)
                            }
                        } else {
                            format!("----- Welcome again, {} -----", name)
                        };

                        server_cmds_channel.push_back(ServerCmd::Announcement(msg));
                    }
                    Command::Message(msg) => {
                        server_cmds_channel.push_back(ServerCmd::Broadcast(
                            event.token(),
                            format!("{}: {}", client.name, msg),
                        ));
                    }
                }
            }
        } else {
            server_cmds_channel.push_back(ServerCmd::Disconnect(event.token()));
        }
    }

    if event.is_writable() {
        if client.stream.flush().is_err() {
            server_cmds_channel.push_back(ServerCmd::Disconnect(event.token()));
        }
    }
}

fn next_token(token_cur: &mut Token) -> Token {
    let next = token_cur.0;
    token_cur.0 += 1;
    Token(next)
}
