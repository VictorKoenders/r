use async_trait::async_trait;
use serde::{de::DeserializeOwned, Serialize};
use std::{future::Future, pin::Pin, sync::Arc};

use crate::{HandleKey, Tagged};

#[async_trait]
pub trait Handler: Serialize + DeserializeOwned + Send {
    type Output: Serialize + DeserializeOwned + Send;
    type Error: Serialize + Send;

    const CAN_HANDLE_DEFAULT: bool = false;
    const CAN_HANDLE_F64: bool = false;

    async fn handle(mut self, ctx: &mut HandleContext) -> Result<Self::Output, Self::Error>;
    fn build_from_default() -> Self {
        unreachable!()
    }
    fn build_from_f64(_: f64) -> Self {
        unreachable!()
    }
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
        let key = HandleKey::from(tag);
        if self.handles.contains_key(&key.0) {
            panic!("Type {:?} has been registered twice", tag);
        }
        println!("Registering tag {:?}", key);
        let wrapper = HandleWrapper {
            can_handle_default: T::CAN_HANDLE_DEFAULT,
            can_handle_f64: T::CAN_HANDLE_F64,
            invoke: Arc::new(|ctx: &mut HandleContext, input: InputTy| {
                Box::pin(async move {
                    let t: T = match input {
                        InputTy::Default => T::build_from_default(),
                        InputTy::F64(val) => T::build_from_f64(val),
                        InputTy::Json(value) => serde_json::from_value(value)
                            .map_err(|source| crate::Error::Deserialize { source })?,
                    };
                    match t.handle(ctx).await {
                        Ok(response) => ctx.respond_with(response).await,
                        Err(e) => ctx.respond_with_error(e).await,
                    }
                    Ok(())
                })
            }),
        };
        self.handles.insert(key, wrapper);
        if !self
            .handles
            .contains_key(&unicase::UniCase::new(String::from("ping")))
        {
            panic!("Type {:?} has not been registered", tag);
        }
        self
    }
}

pub trait HandleInvokeFnResult: Future<Output = Result<(), crate::Error>> + Send {}

impl<T> HandleInvokeFnResult for T where T: Future<Output = Result<(), crate::Error>> + Send {}

pub trait HandleInvokeFn:
    for<'a> Fn(&'a mut HandleContext, InputTy) -> Pin<Box<dyn HandleInvokeFnResult + 'a>> + Send + Sync
{
}

impl<T> HandleInvokeFn for T where
    T: for<'a> Fn(&'a mut HandleContext, InputTy) -> Pin<Box<dyn HandleInvokeFnResult + 'a>>
        + Send
        + Sync
{
}

#[derive(Clone)]
pub struct HandleWrapper {
    pub can_handle_default: bool,
    pub can_handle_f64: bool,
    pub invoke: Arc<dyn HandleInvokeFn>,
}

static_assertions::assert_impl_all!(HandleWrapper: Send);

pub enum InputTy {
    Json(serde_json::Value),
    F64(f64),
    Default,
}
