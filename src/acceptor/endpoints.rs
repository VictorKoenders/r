pub(super) mod tcp;

use async_trait::async_trait;
use futures::channel::mpsc::Sender;

#[async_trait]
pub trait Endpoint {
    async fn send(&mut self, addr: EndpointAddr, message: &str);
}

#[async_trait]
pub trait EndpointBuilder {
    async fn build(&mut self, sender: Sender<IncomingMessage>) -> super::Result<Box<dyn Endpoint>>;
}

pub struct IncomingMessage {
    pub address: EndpointAddr,
    pub message: String,
}

#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum EndpointAddr {
    Tcp {
        endpoint_index: usize,
        client_index: usize,
    },
}
