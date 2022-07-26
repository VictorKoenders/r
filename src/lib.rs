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
use itertools::free::join;
use std::str::FromStr;
use std::{collections::HashMap, time::Instant};
use unicase::UniCase;

#[derive(Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
struct HandleKey(UniCase<String>);

impl HandleKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<UniCase<String>> for HandleKey {
    fn borrow(&self) -> &UniCase<String> {
        &self.0
    }
}

impl std::fmt::Debug for HandleKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_str().fmt(f)
    }
}

impl From<&'static str> for HandleKey {
    fn from(inner: &'static str) -> Self {
        let key = join(inner.split("::").skip(1), ".");
        println!("Key = {:?}", key);
        Self(UniCase::new(key))
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
                    // try to parse the incoming tag as JSON
                    let (id, tag, value) = if let Ok(Tagged {
                        id,
                        tag,
                        value,
                        error: _,
                    }) = serde_json::from_str(&message)
                    {
                        (id, tag, value)
                        // else parse e.g. "temp.record;12.34"
                    } else if let Some((tag, remaining)) = message.split_once(';') {
                        let value: serde_json::Value = if let Ok(val) = f64::from_str(remaining) {
                            val.into()
                        } else {
                            serde_json::Value::String(remaining.to_owned())
                        };
                        (String::new(), tag.to_owned(), value)
                        // else assume the entire message is a tag, e.g. "Ping"
                    } else {
                        (String::new(), message, serde_json::Value::Null)
                    };

                    let handle = self.handles.get(&UniCase::new(tag.clone()));
                    async_std::task::spawn({
                        let mut sender = self.task_finished_sender.clone();
                        let handle = handle.cloned();
                        async move {
                            let start = Instant::now();
                            let mut ctx = HandleContext::new(id);
                            if let Some(handle) = handle {
                                if let Err(e) = spawn_handle(handle, value, &mut ctx).await {
                                    ctx.respond_with_error(e).await;
                                }
                            } else {
                                ctx.respond_with_error(format!("Handler {:?} not found", tag))
                                    .await;
                            }
                            println!("{:?} done {:?}", tag, start.elapsed());
                            let _ = sender.send((address, ctx)).await;
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

async fn spawn_handle(
    handle: HandleWrapper,
    value: serde_json::Value,
    ctx: &mut HandleContext,
) -> Result<()> {
    if value.is_null() && handle.can_handle_default {
        (handle.invoke)(ctx, InputTy::Default).await
    } else if handle.can_handle_f64 && value.is_f64() {
        if let Some(val) = value.as_f64() {
            (handle.invoke)(ctx, InputTy::F64(val)).await
        } else {
            unreachable!()
        }
    } else {
        (handle.invoke)(ctx, InputTy::Json(value)).await
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
