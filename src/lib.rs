mod acceptor;
mod handler;

pub use acceptor::*;
pub use handler::*;

use async_std::prelude::StreamExt;
use futures::{
    channel::mpsc::{channel, Receiver, Sender},
    future::Either,
    SinkExt,
};
use std::{collections::HashMap, time::Instant};

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct HandleKey(&'static str);

impl std::borrow::Borrow<str> for HandleKey {
    fn borrow(&self) -> &str {
        self.0
    }
}

impl From<&'static str> for HandleKey {
    fn from(inner: &'static str) -> Self {
        Self(inner)
    }
}

pub struct System {
    endpoints: Vec<Box<dyn Endpoint>>,
    handles: HashMap<HandleKey, HandleWrapper>,
    receiver_incoming_message: Receiver<IncomingMessage>,
    task_finished_receiver: Receiver<(EndpointAddr, HandleContext)>,
    task_finished_sender: Sender<(EndpointAddr, HandleContext)>,
}

impl System {
    pub fn builder() -> SystemBuilder {
        SystemBuilder {
            endpoint_builders: Vec::new(),
            handles: HashMap::new(),
        }
    }

    pub async fn run(mut self) {
        let mut stream = futures::stream::select(
            self.receiver_incoming_message.map(Either::Left),
            self.task_finished_receiver.map(Either::Right),
        );
        while let Some(message) = stream.next().await {
            match message {
                Either::Left(IncomingMessage { address, message }) => {
                    let Tagged {
                        id,
                        tag,
                        value,
                        error: _,
                    } = match serde_json::from_str(&message) {
                        Ok(tagged) => tagged,
                        Err(e) => {
                            println!("Could not deserialize message: {:?}", e);
                            println!("{}", message);
                            return;
                        }
                    };
                    let handle = self.handles.get(tag.as_str());
                    async_std::task::spawn({
                        let mut sender = self.task_finished_sender.clone();
                        let handle = handle.cloned();
                        async move {
                            let start = Instant::now();
                            let mut ctx = HandleContext::new(id);
                            if let Some(handle) = handle {
                                if let Err(e) = (handle.invoke)(&mut ctx, value).await {
                                    eprintln!("Could not handle {}", handle.tag);
                                    eprintln!("{:?}", e);
                                } else {
                                    println!(
                                        "Executed task {} in {:?}",
                                        handle.tag,
                                        start.elapsed()
                                    );
                                }
                            } else {
                                ctx.respond_with_error("No valid handler found").await;
                            };
                            sender
                                .send((address, ctx))
                                .await
                                .expect("Could not report task being finished");
                        }
                    });
                }
                Either::Right((address, ctx)) => {
                    for msg in ctx.take_messages() {
                        for endpoint in &mut self.endpoints {
                            endpoint.send(address.clone(), &msg).await;
                        }
                    }
                }
            }
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Tagged {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub tag: String,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub value: serde_json::Value,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub error: serde_json::Value,
}

pub struct SystemBuilder {
    endpoint_builders: Vec<Box<dyn EndpointBuilder>>,
    handles: HashMap<HandleKey, HandleWrapper>,
}

impl SystemBuilder {
    pub async fn build(self) -> Result<System> {
        let (sender_incoming_message, receiver_incoming_message) = channel(1024);
        let mut endpoints = Vec::with_capacity(self.endpoint_builders.len());
        for mut builder in self.endpoint_builders {
            endpoints.push(builder.build(sender_incoming_message.clone()).await?);
        }
        let (task_finished_sender, task_finished_receiver) = channel(1024);
        Ok(System {
            endpoints,
            handles: self.handles,
            receiver_incoming_message,
            task_finished_receiver,
            task_finished_sender,
        })
    }
}
