mod endpoints;

pub use self::endpoints::{Endpoint, EndpointAddr, EndpointBuilder, IncomingMessage};
use async_std::net::ToSocketAddrs;

impl crate::SystemBuilder {
    pub fn accept_tcp<T>(mut self, addr: T) -> Self
    where
        T: ToSocketAddrs + Send + 'static,
        for<'a> &'a T: Send,
        <T as ToSocketAddrs>::Iter: Send,
    {
        self.endpoint_builders
            .push(Box::new(endpoints::tcp::TcpBuilder(addr)));
        self
    }
}

pub use endpoints::tcp::TcpError;

#[non_exhaustive]
#[derive(Debug)]
pub enum Error {
    Tcp(TcpError),
    Deserialize { source: serde_json::Error },
}

pub type Result<T = ()> = std::result::Result<T, Error>;
