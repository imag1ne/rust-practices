#![allow(unused_imports)]
use async_std::net::ToSocketAddrs;
use async_std::sync::Mutex;
use async_std::task::TaskId;
use async_std::{net::TcpListener, net::TcpStream, prelude::*, task};
use chatlib::async_stream_agent::AsyncStreamAgent;
use chatlib::Command;
use futures::channel::mpsc;
use futures::stream::SplitSink;
use futures::{
    executor::{LocalPool, LocalSpawner},
    task::SpawnExt,
    SinkExt, StreamExt,
};
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::{env, io};

type Sender<T> = mpsc::UnboundedSender<T>;
type Receiver<T> = mpsc::UnboundedReceiver<T>;

pub enum ServerCmd {
    Announcement(String),
    Broadcast(TaskId, String),
    Connect(TcpStream),
    Disconnect(TaskId),
}

pub struct Client {
    name: Mutex<String>,
    stream: Mutex<SplitSink<AsyncStreamAgent<TcpStream>, Command>>,
}

impl Client {
    pub fn new(stream: SplitSink<AsyncStreamAgent<TcpStream>, Command>) -> Self {
        Self {
            name: Default::default(),
            stream: Mutex::new(stream),
        }
    }

    pub async fn name(&self) -> String {
        let name = self.name.lock().await;

        (*name).clone()
    }

    pub async fn set_name(&self, name: &str) {
        let new_name = name.to_string();
        let mut name = self.name.lock().await;
        *name = new_name;
    }

    pub async fn send_msg(&self, msg: &str) -> io::Result<()> {
        self.stream
            .lock()
            .await
            .send(Command::Message(msg.to_string()))
            .await?;
        Ok(())
    }
}

pub struct Clients(HashMap<TaskId, Arc<Client>>);

impl Clients {
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    pub async fn notify_all(&mut self, msg: &str, sx: &mut Sender<ServerCmd>) {
        for (id, client) in &self.0 {
            if client.send_msg(msg).await.is_err() {
                sx.send(ServerCmd::Disconnect(*id)).await.unwrap();
            }
        }
    }

    pub async fn notify_all_exclude(
        &mut self,
        msg: &str,
        sx: &mut Sender<ServerCmd>,
        exclude: TaskId,
    ) {
        let to = self.0.iter().filter(|(&id, _)| id != exclude);
        for (id, client) in to {
            if client.send_msg(msg).await.is_err() {
                sx.send(ServerCmd::Disconnect(*id)).await.unwrap();
            }
        }
    }
}

impl Deref for Clients {
    type Target = HashMap<TaskId, Arc<Client>>;

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

    task::block_on(accept_loop(addr))?;

    Ok(())
}

pub async fn accept_loop(addr: impl ToSocketAddrs) -> io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    let (mut sx, rx) = mpsc::unbounded();

    let broker_handle = task::spawn(broker_loop(sx.clone(), rx));

    while let Some(stream) = listener.incoming().next().await {
        match stream {
            Ok(stream) => sx
                .send(ServerCmd::Connect(stream))
                .await
                .expect("failed to send server command"),
            Err(e) => eprintln!("{}", e),
        }
    }

    drop(sx);
    broker_handle.await;

    Ok(())
}

pub async fn broker_loop(
    mut server_cmd_sx: Sender<ServerCmd>,
    mut server_cmd_rx: Receiver<ServerCmd>,
) {
    let mut clients = Clients::new();
    while let Some(server_cmd) = server_cmd_rx.next().await {
        match server_cmd {
            ServerCmd::Announcement(msg) => clients.notify_all(&msg, &mut server_cmd_sx).await,
            ServerCmd::Broadcast(src_id, msg) => {
                clients
                    .notify_all_exclude(&msg, &mut server_cmd_sx, src_id)
                    .await
            }
            ServerCmd::Connect(stream) => {
                let mut server_cmd_sx = server_cmd_sx.clone();
                let (wt_stream, mut rd_stream) = AsyncStreamAgent::new(stream).split();

                let client = Arc::new(Client::new(wt_stream));
                let client_clone = client.clone();

                let id = task::spawn(async move {
                    let id = task::current().id();
                    while let Some(cmd_res) = rd_stream.next().await {
                        match cmd_res {
                            Ok(cmd) => match cmd {
                                Command::Name(name) => {
                                    let old_name = client.name().await.clone();
                                    let msg = if old_name != name {
                                        client.set_name(&name).await;
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

                                    server_cmd_sx
                                        .send(ServerCmd::Announcement(msg))
                                        .await
                                        .unwrap()
                                }
                                Command::Message(msg) => {
                                    server_cmd_sx
                                        .send(ServerCmd::Broadcast(
                                            id,
                                            format!("{}: {}", client.name().await, msg),
                                        ))
                                        .await
                                        .unwrap();
                                }
                            },
                            Err(_) => server_cmd_sx.send(ServerCmd::Disconnect(id)).await.unwrap(),
                        }
                    }
                })
                .task()
                .id();

                clients.insert(id, client_clone);
            }
            ServerCmd::Disconnect(id) => {
                if let Some(client) = clients.remove(&id) {
                    server_cmd_sx
                        .send(ServerCmd::Announcement(format!(
                            "----- {} has left -----",
                            client.name().await
                        )))
                        .await
                        .unwrap();
                }
            }
        }
    }
}
