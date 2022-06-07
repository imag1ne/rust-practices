use chatlib::{Command, Deserialize, Serialize};
use std::collections::HashMap;
use std::net::TcpStream;
use std::ops::{Deref, DerefMut};
use std::sync::mpsc::Sender;
use std::sync::{mpsc, Arc, Mutex};
use std::thread::ThreadId;
use std::{env, io, net::TcpListener, thread};

pub enum ServerCmd {
    Announcement(String),
    Broadcast(ThreadId, String),
    Connect(TcpStream),
    Disconnect(ThreadId),
}

pub struct Client {
    name: Mutex<String>,
    stream: TcpStream,
}

impl Client {
    pub fn new(stream: TcpStream) -> Self {
        Self {
            name: Default::default(),
            stream,
        }
    }

    pub fn stream(&self) -> &TcpStream {
        &self.stream
    }

    pub fn name(&self) -> String {
        let name = self
            .name
            .lock()
            .expect("failed to get the lock of Client.name");
        name.clone()
    }

    pub fn set_name(&self, name: &str) {
        let new_name = name.to_string();
        let mut name = self
            .name
            .lock()
            .expect("failed to get the lock of Client.name");
        *name = new_name;
    }

    pub fn send_msg(&self, msg: &str) -> io::Result<()> {
        self.stream().encode(Command::Message(msg.to_string()))?;
        Ok(())
    }
}

pub struct Clients(HashMap<ThreadId, Arc<Client>>);

impl Clients {
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    pub fn notify_all(&self, msg: &str, sx: &Sender<ServerCmd>) {
        for (id, client) in &self.0 {
            if client.send_msg(msg).is_err() {
                sx.send(ServerCmd::Disconnect(*id)).unwrap();
            }
        }
    }

    pub fn notify_all_exclude(&self, msg: &str, sx: &Sender<ServerCmd>, exclude: ThreadId) {
        let to = self.0.iter().filter(|(&id, _)| id != exclude);
        for (id, client) in to {
            if client.send_msg(msg).is_err() {
                sx.send(ServerCmd::Disconnect(*id)).unwrap();
            }
        }
    }
}

impl Deref for Clients {
    type Target = HashMap<ThreadId, Arc<Client>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Clients {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

fn main() -> io::Result<()> {
    let addr = env::args().nth(1).unwrap_or(String::from("127.0.0.1:8000"));
    let listener = TcpListener::bind(addr)?;
    let mut clients = Clients::new();

    let (sx, rx) = mpsc::channel();

    let server_cmd_sx = sx.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => server_cmd_sx
                    .send(ServerCmd::Connect(stream))
                    .expect("failed to send server command"),
                Err(e) => eprintln!("{}", e),
            }
        }
    });

    for server_cmd in rx {
        match server_cmd {
            ServerCmd::Announcement(msg) => clients.notify_all(&msg, &sx),
            ServerCmd::Broadcast(src_id, msg) => clients.notify_all_exclude(&msg, &sx, src_id),
            ServerCmd::Connect(stream) => {
                let server_cmd_sx = sx.clone();
                let client = Arc::new(Client::new(stream));
                let client_clone = client.clone();

                let client_id = thread::spawn(move || {
                    let id = thread::current().id();
                    while let Ok(cmd) = client.stream().decode() {
                        match cmd {
                            Command::Name(name) => {
                                let old_name = client.name();
                                let msg = if old_name != name {
                                    client.set_name(&name);
                                    if old_name.is_empty() {
                                        format!("----- Welcome, {} -----", name)
                                    } else {
                                        format!(
                                            "----- {} changed name to 👉 {} -----",
                                            old_name, name
                                        )
                                    }
                                } else {
                                    format!("----- Welcome again, {} -----", name)
                                };

                                server_cmd_sx.send(ServerCmd::Announcement(msg)).unwrap();
                            }
                            Command::Message(msg) => {
                                server_cmd_sx
                                    .send(ServerCmd::Broadcast(
                                        id,
                                        format!("{}: {}", client.name(), msg),
                                    ))
                                    .unwrap();
                            }
                        }
                    }

                    server_cmd_sx.send(ServerCmd::Disconnect(id)).unwrap();
                })
                .thread()
                .id();

                clients.insert(client_id, client_clone);
            }
            ServerCmd::Disconnect(id) => {
                if let Some(client) = clients.remove(&id) {
                    sx.send(ServerCmd::Announcement(format!(
                        "----- {} has left -----",
                        client.name()
                    )))
                    .unwrap();
                }
            }
        }
    }

    Ok(())
}
