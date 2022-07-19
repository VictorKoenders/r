use super::{Endpoint, EndpointAddr, EndpointBuilder, IncomingMessage};
use async_std::{
    future::timeout,
    io::{prelude::BufReadExt, BufReader, BufWriter, WriteExt},
    net::{TcpListener, TcpStream, ToSocketAddrs},
    task::JoinHandle,
};
use async_trait::async_trait;
use futures::{
    channel::mpsc::{channel, Receiver, SendError, Sender},
    future::Either,
    AsyncReadExt, SinkExt, StreamExt,
};
use snafu::{ResultExt, Snafu};
use std::{collections::HashMap, net::SocketAddr, time::Duration};

struct TcpEndpoints {
    handles: Vec<JoinHandle<Result<(), TcpError>>>,
    runner: JoinHandle<()>,
    to_runner: Sender<ToRunner>,
}

#[async_trait]
impl Endpoint for TcpEndpoints {
    async fn send(&mut self, addr: super::EndpointAddr, message: &str) {
        #[allow(irrefutable_let_patterns)] // we'll be adding more endpoints in the future
        if let EndpointAddr::Tcp {
            endpoint_index,
            client_index,
        } = addr
        {
            self.to_runner
                .send(ToRunner::Message {
                    endpoint_index,
                    client_index,
                    message: message.to_owned(),
                })
                .await
                .expect("TCP endpoint runner shut down; could not deliver message");
        }
    }
}

impl Drop for TcpEndpoints {
    fn drop(&mut self) {
        async_std::task::block_on(async move {
            self.to_runner
                .send(ToRunner::Shutdown)
                .await
                .expect("Could not shutdown TCP runner");
            timeout(Duration::from_secs(1), &mut self.runner)
                .await
                .expect("TCP runner did not shut down in time");
            for handle in std::mem::take(&mut self.handles) {
                timeout(Duration::from_secs(1), handle)
                    .await
                    .expect("TCP endpoint handle did not shut down in time")
                    .expect("TCP endpoint encountered an error");
            }
        });
    }
}

pub struct TcpBuilder<T: ToSocketAddrs>(pub T);

#[async_trait]
impl<T> EndpointBuilder for TcpBuilder<T>
where
    T: ToSocketAddrs + Send + 'static,
    for<'a> &'a T: Send,
    <T as ToSocketAddrs>::Iter: Send,
{
    async fn build(
        &mut self,
        incoming_message_sender: Sender<IncomingMessage>,
    ) -> crate::acceptor::Result<Box<dyn Endpoint>> {
        let mut listeners = Vec::new();
        for addr in self
            .0
            .to_socket_addrs()
            .await
            .map_err(|source| crate::Error::Tcp(TcpError::CouldNotResolveAddr { source }))?
        {
            listeners.push((
                addr,
                TcpListener::bind(addr).await.map_err(|source| {
                    crate::Error::Tcp(TcpError::CouldNotBindListener { addr, source })
                })?,
            ));
        }
        let (sender, receiver) = channel(1024);
        let handles = listeners
            .into_iter()
            .enumerate()
            .map(|(index, (addr, listener))| {
                async_std::task::spawn(run(index, addr, listener, sender.clone()))
            })
            .collect();
        let (to_runner_sender, to_runner_receiver) = channel(1024);
        let runner = async_std::task::spawn(runner(
            receiver,
            incoming_message_sender,
            to_runner_receiver,
        ));
        Ok(Box::new(TcpEndpoints {
            handles,
            runner,
            to_runner: to_runner_sender,
        }) as Box<dyn Endpoint>)
    }
}

struct ClientState {
    sender: Sender<ToTcpStream>,
    addr: SocketAddr,
}

async fn runner(
    from_tcp_listener_receiver: Receiver<FromTcpListener>,
    mut sender: Sender<IncomingMessage>,
    to_runner_receiver: Receiver<ToRunner>,
) {
    let mut handles = HashMap::<(usize, usize), ClientState>::new();
    let mut stream = futures::stream::select(
        from_tcp_listener_receiver.map(Either::Left),
        to_runner_receiver.map(Either::Right),
    );
    while let Some(msg) = stream.next().await {
        match msg {
            Either::Left(FromTcpListener::ClientError {
                client_index,
                endpoint_index,
                source,
            }) => {
                if let Some(client) = handles.remove(&(endpoint_index, client_index)) {
                    eprintln!(
                        "Client {} ({}:{}) died: {:?}",
                        client.addr, endpoint_index, client_index, source
                    );
                }
            }
            Either::Left(FromTcpListener::ClientReceive {
                endpoint_index,
                client_index,
                line,
            }) => {
                let _ = sender
                    .send(IncomingMessage {
                        address: super::EndpointAddr::Tcp {
                            endpoint_index,
                            client_index,
                        },
                        message: line,
                    })
                    .await;
            }
            Either::Left(FromTcpListener::IncomingConnection {
                endpoint_index,
                client_index,
                address,
                stream_sender,
            }) => {
                println!(
                    "New connection {} ({}:{})",
                    address, endpoint_index, client_index
                );
                handles.insert(
                    (endpoint_index, client_index),
                    ClientState {
                        addr: address,
                        sender: stream_sender,
                    },
                );
            }
            Either::Right(ToRunner::Message {
                endpoint_index,
                client_index,
                message,
            }) => {
                if let Some(client) = handles.get_mut(&(endpoint_index, client_index)) {
                    if client
                        .sender
                        .send(ToTcpStream::Send(message))
                        .await
                        .is_err()
                    {
                        eprintln!(
                            "TCP client {} ({}:{}) shut down",
                            client.addr, endpoint_index, client_index
                        );
                        handles.remove(&(endpoint_index, client_index));
                    }
                }
            }
            Either::Right(ToRunner::Shutdown) => {
                for (_, mut client) in handles {
                    // If we fail to deliver a message to this client, it's already shut down
                    // we're shutting down the runner so we don't care about this error.
                    let _ = client.sender.send(ToTcpStream::Exit).await;
                }
                break;
            }
        }
    }
}

async fn run(
    endpoint_index: usize,
    addr: SocketAddr,
    listener: TcpListener,
    mut sender: Sender<FromTcpListener>,
) -> Result<(), TcpError> {
    let mut client_index = 0;
    loop {
        let (socket, address) = listener
            .accept()
            .await
            .context(CouldNotAcceptClientSnafu { listen_addr: addr })?;
        println!("Received connection on {:?}", addr);
        let (stream_sender, stream_receiver) = channel(1024);
        sender
            .send(FromTcpListener::IncomingConnection {
                endpoint_index,
                client_index,
                address,
                stream_sender,
            })
            .await
            .context(SenderClosedSnafu)?;
        async_std::task::spawn({
            let mut sender = sender.clone();
            async move {
                if let Err(source) = run_client(
                    stream_receiver,
                    sender.clone(),
                    socket,
                    address,
                    endpoint_index,
                    client_index,
                )
                .await
                {
                    let _ = sender
                        .send(FromTcpListener::ClientError {
                            endpoint_index,
                            client_index,
                            source,
                        })
                        .await;
                }
            }
        });
        client_index += 1;
    }
}

async fn run_client(
    receiver: Receiver<ToTcpStream>,
    mut sender: Sender<FromTcpListener>,
    socket: TcpStream,
    address: SocketAddr,
    endpoint_index: usize,
    client_index: usize,
) -> Result<(), TcpError> {
    let (read, write) = socket.split();
    let reader = BufReader::new(read).lines();
    let mut writer = BufWriter::new(write);
    let mut joined = futures::stream::select(reader.map(Either::Left), receiver.map(Either::Right));
    loop {
        match joined.next().await {
            Some(Either::Left(line)) => {
                let line = line.context(StreamCouldNotReadLineSnafu { address })?;
                sender
                    .send(FromTcpListener::ClientReceive {
                        client_index,
                        endpoint_index,
                        line,
                    })
                    .await
                    .context(SenderClosedSnafu)?;
            }
            Some(Either::Right(ToTcpStream::Exit)) => break Ok(()),
            Some(Either::Right(ToTcpStream::Send(msg))) => {
                writer
                    .write_all(msg.as_bytes())
                    .await
                    .context(StreamCouldNotSendSnafu { address })?;
                writer
                    .write_all(b"\r\n")
                    .await
                    .context(StreamCouldNotSendSnafu { address })?;
                writer
                    .flush()
                    .await
                    .context(StreamCouldNotSendSnafu { address })?;
            }
            // This case should never be hit
            None => break Ok(()),
        }
    }
}

#[non_exhaustive]
#[derive(Snafu, Debug)]
pub enum TcpError {
    CouldNotResolveAddr {
        source: std::io::Error,
    },
    CouldNotBindListener {
        source: std::io::Error,
        addr: SocketAddr,
    },
    CouldNotAcceptClient {
        source: std::io::Error,
        listen_addr: SocketAddr,
    },
    SenderClosed {
        source: SendError,
    },
    StreamCouldNotReadLine {
        address: SocketAddr,
        source: std::io::Error,
    },
    StreamCouldNotSend {
        address: SocketAddr,
        source: std::io::Error,
    },
}

#[non_exhaustive]
#[derive(Debug)]
pub enum ToRunner {
    Message {
        endpoint_index: usize,
        client_index: usize,
        message: String,
    },
    Shutdown,
}

#[non_exhaustive]
#[derive(Debug)]
pub enum FromTcpListener {
    IncomingConnection {
        endpoint_index: usize,
        client_index: usize,
        address: SocketAddr,
        stream_sender: Sender<ToTcpStream>,
    },
    ClientError {
        endpoint_index: usize,
        client_index: usize,
        source: TcpError,
    },
    ClientReceive {
        endpoint_index: usize,
        client_index: usize,
        line: String,
    },
}

#[non_exhaustive]
#[derive(Debug)]
pub enum ToTcpStream {
    Send(String),
    Exit,
}
