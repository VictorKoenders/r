use async_trait::async_trait;
use serde::{de::DeserializeOwned, Serialize};
use std::{future::Future, pin::Pin, sync::Arc};

use crate::{HandleKey, Tagged};

#[async_trait]
pub trait Handler<STATE = ()>: DeserializeOwned + Send {
    type Error: Serialize + Send;

    const CAN_HANDLE_DEFAULT: bool = false;
    const CAN_HANDLE_STRING: bool = false;
    const CAN_HANDLE_F64: bool = false;

    async fn handle(mut self, state: STATE, ctx: &mut HandleContext) -> Result<(), Self::Error>;
    fn build_from_default() -> Self {
        unreachable!()
    }
    fn build_from_string(_: String) -> Self {
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

    pub fn reply<T>(&mut self, msg: T)
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
    pub fn error<T>(&mut self, error: T)
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
    pub fn error_string(&mut self, error: impl ToString) {
        let error = serde_json::Value::String(error.to_string());
        let mut message = serde_json::to_string(&Tagged {
            id: self.id.clone(),
            tag: String::new(),
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

impl<STATE: Clone + Send + 'static> super::SystemBuilder<STATE> {
    pub fn handle<T>(mut self) -> Self
    where
        T: Handler<STATE>,
    {
        let tag = std::any::type_name::<T>();
        let key = HandleKey::from(tag);
        if self.handles.contains_key(&key.0) {
            panic!("Type {:?} has been registered twice", tag);
        }
        println!("Registering tag {:?}", key);
        let wrapper = HandleWrapper {
            can_handle_default: T::CAN_HANDLE_DEFAULT,
            can_handle_string: T::CAN_HANDLE_STRING,
            can_handle_f64: T::CAN_HANDLE_F64,
            invoke: Arc::new(|state: STATE, ctx: &mut HandleContext, input: InputTy| {
                Box::pin(async move {
                    let t: T = match input {
                        InputTy::Default => T::build_from_default(),
                        InputTy::F64(val) => T::build_from_f64(val),
                        InputTy::String(val) => T::build_from_string(val),
                        InputTy::Json(value) => serde_json::from_value(value)
                            .map_err(|source| crate::Error::Deserialize { source })?,
                    };
                    if let Err(e) = t.handle(state, ctx).await {
                        ctx.error(e);
                    }
                    Ok(())
                })
            }),
        };
        self.handles.insert(key, wrapper);
        self
    }
}

pub trait HandleInvokeFnResult: Future<Output = Result<(), crate::Error>> + Send {}

impl<T> HandleInvokeFnResult for T where T: Future<Output = Result<(), crate::Error>> + Send {}

pub trait HandleInvokeFn<STATE>:
    for<'a> Fn(STATE, &'a mut HandleContext, InputTy) -> Pin<Box<dyn HandleInvokeFnResult + 'a>>
    + Send
    + Sync
{
}

impl<STATE, T> HandleInvokeFn<STATE> for T where
    T: for<'a> Fn(STATE, &'a mut HandleContext, InputTy) -> Pin<Box<dyn HandleInvokeFnResult + 'a>>
        + Send
        + Sync
{
}

#[derive(Clone)]
pub struct HandleWrapper<STATE> {
    pub can_handle_default: bool,
    pub can_handle_string: bool,
    pub can_handle_f64: bool,
    pub invoke: Arc<dyn HandleInvokeFn<STATE>>,
}

pub enum InputTy {
    Json(serde_json::Value),
    String(String),
    F64(f64),
    Default,
}
