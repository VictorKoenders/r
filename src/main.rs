use async_trait::async_trait;
use r::{HandleContext, Handler};

#[async_std::main]
async fn main() {
    let system = r::System::builder()
        .accept_tcp("0.0.0.0:1234")
        .handle::<Ping>()
        .build()
        .await
        .expect("Could not build system");

    system.run().await;
}

#[derive(serde::Deserialize, serde::Serialize)]
struct Ping {}

#[async_trait]
impl Handler for Ping {
    type Output = Pong;
    type Error = ();
    async fn handle(mut self, _ctx: &mut HandleContext) -> Result<Pong, ()> {
        Ok(Pong {
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        })
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
pub struct Pong {
    timestamp: u64,
}
