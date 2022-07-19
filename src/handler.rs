use async_trait::async_trait;
use serde::{de::DeserializeOwned, Serialize};
use std::{future::Future, pin::Pin, sync::Arc};

use crate::Tagged;

#[async_trait]
pub trait Handler: Serialize + DeserializeOwned + Send {
    type Output: Serialize + DeserializeOwned + Send;
    type Error: Serialize + Send;

    async fn handle(mut self, ctx: &mut HandleContext) -> Result<Self::Output, Self::Error>;
}

pub struct HandleContext {
    id: String,
    messages: Vec<String>,
}

impl HandleContext {
    pub fn new(id: String) -> Self {
        Self {
            id,
            messages: Vec::new(),
        }
    }

    pub async fn respond_with<T>(&mut self, msg: T)
    where
        T: Serialize,
    {
        let value = serde_json::to_value(&msg).expect("Could not serialize message");
        let mut message = serde_json::to_string(&Tagged {
            id: self.id.clone(),
            tag: std::any::type_name::<T>().to_string(),
            value,
            error: serde_json::Value::Null,
        })
        .unwrap();
        message += "\r\n";
        self.messages.push(message);
    }
    pub async fn respond_with_error<T>(&mut self, error: T)
    where
        T: Serialize,
    {
        let error = serde_json::to_value(&error).expect("Could not serialize message");
        let mut message = serde_json::to_string(&Tagged {
            id: self.id.clone(),
            tag: std::any::type_name::<T>().to_string(),
            value: serde_json::Value::Null,
            error,
        })
        .unwrap();
        message += "\r\n";
        self.messages.push(message);
    }

    pub fn take_messages(self) -> Vec<String> {
        self.messages
    }
}

impl super::SystemBuilder {
    pub fn handle<T>(mut self) -> Self
    where
        T: Handler,
    {
        let tag = std::any::type_name::<T>();
        if self.handles.contains_key(tag) {
            panic!("Type {:?} has been registered twice", tag);
        }
        println!("Registering tag {:?}", tag);
        let wrapper = HandleWrapper {
            tag,
            invoke: Arc::new(|ctx: &mut HandleContext, value: serde_json::Value| {
                Box::pin(async move {
                    let deserialized: T = serde_json::from_value(value)
                        .map_err(|source| crate::Error::Deserialize { source })?;
                    match deserialized.handle(ctx).await {
                        Ok(response) => ctx.respond_with(response).await,
                        Err(e) => ctx.respond_with_error(e).await,
                    }
                    Ok(())
                })
            }),
        };
        self.handles.insert(tag.into(), wrapper);
        self
    }
}

pub trait HandleInvokeFnResult: Future<Output = Result<(), crate::Error>> + Send {}

impl<T> HandleInvokeFnResult for T where T: Future<Output = Result<(), crate::Error>> + Send {}

pub trait HandleInvokeFn:
    for<'a> Fn(&'a mut HandleContext, serde_json::Value) -> Pin<Box<dyn HandleInvokeFnResult + 'a>>
    + Send
    + Sync
{
}

impl<T> HandleInvokeFn for T where
    T: for<'a> Fn(
            &'a mut HandleContext,
            serde_json::Value,
        ) -> Pin<Box<dyn HandleInvokeFnResult + 'a>>
        + Send
        + Sync
{
}

#[derive(Clone)]
pub struct HandleWrapper {
    pub tag: &'static str,
    pub invoke: Arc<dyn HandleInvokeFn>,
}

static_assertions::assert_impl_all!(HandleWrapper: Send);
