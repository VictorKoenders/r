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

pub mod prelude {
    pub use crate::{HandleContext, Handler};
    pub use async_trait::async_trait;
    pub use serde;

    pub struct SerializeAsDebugString<T: std::fmt::Debug>(pub T);

    impl<T: std::fmt::Debug> serde::Serialize for SerializeAsDebugString<T> {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            let str = format!("{:?}", self.0);
            str.serialize(serializer)
        }
    }
}

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
        Self(UniCase::new(key))
    }
}

pub struct System<STATE: Clone + Send + 'static> {
    endpoints: Vec<Box<dyn Endpoint>>,
    handles: HashMap<HandleKey, HandleWrapper<STATE>>,
    receiver_incoming_message: Receiver<IncomingMessage>,
    task_finished_receiver: Receiver<(EndpointAddr, HandleContext)>,
    task_finished_sender: Sender<(EndpointAddr, HandleContext)>,
    state: STATE,
}

impl System<()> {
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> SystemBuilder<()> {
        SystemBuilder {
            endpoint_builders: Vec::new(),
            handles: HashMap::new(),
            state: (),
        }
    }
}

impl<STATE: Clone + Send + 'static> System<STATE> {
    pub fn with_state(state: STATE) -> SystemBuilder<STATE> {
        SystemBuilder {
            endpoint_builders: Vec::new(),
            handles: HashMap::new(),
            state,
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
                        let state = self.state.clone();
                        async move {
                            let start = Instant::now();
                            let mut ctx = HandleContext::new(id);
                            if let Some(handle) = handle {
                                if let Err(e) = spawn_handle(handle, state, value, &mut ctx).await {
                                    ctx.error(e);
                                }
                            } else {
                                ctx.error_string(format!("Handler {:?} not found", tag));
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

async fn spawn_handle<STATE>(
    handle: HandleWrapper<STATE>,
    state: STATE,
    value: serde_json::Value,
    ctx: &mut HandleContext,
) -> Result<()> {
    let input_ty = if value.is_null() && handle.can_handle_default {
        InputTy::Default
    } else if handle.can_handle_f64 && value.is_f64() {
        if let Some(val) = value.as_f64() {
            InputTy::F64(val)
        } else {
            unreachable!()
        }
    } else if handle.can_handle_string && value.is_string() {
        if let Some(val) = value.as_str() {
            InputTy::String(val.to_owned())
        } else {
            unreachable!()
        }
    } else {
        InputTy::Json(value)
    };
    (handle.invoke)(state, ctx, input_ty).await
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

pub struct SystemBuilder<STATE> {
    endpoint_builders: Vec<Box<dyn EndpointBuilder>>,
    handles: HashMap<HandleKey, HandleWrapper<STATE>>,
    state: STATE,
}

impl<STATE: Clone + Send + 'static> SystemBuilder<STATE> {
    pub async fn build(self) -> Result<System<STATE>> {
        let SystemBuilder {
            endpoint_builders,
            handles,
            state,
        } = self;

        let (sender_incoming_message, receiver_incoming_message) = channel(1024);
        let mut endpoints = Vec::with_capacity(endpoint_builders.len());
        for mut builder in endpoint_builders {
            endpoints.push(builder.build(sender_incoming_message.clone()).await?);
        }
        let (task_finished_sender, task_finished_receiver) = channel(1024);
        Ok(System {
            endpoints,
            handles,
            receiver_incoming_message,
            task_finished_receiver,
            task_finished_sender,
            state,
        })
    }
}
