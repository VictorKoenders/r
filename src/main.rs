use r::prelude::*;

#[async_std::main]
async fn main() {
    let system = r::System::new()
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
    type Error = ();
    async fn handle(mut self, _state: (), ctx: &mut HandleContext) -> Result<(), ()> {
        ctx.reply(Pong {
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        });
        Ok(())
    }

    const CAN_HANDLE_DEFAULT: bool = true;
    fn build_from_default() -> Self {
        Self {}
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
pub struct Pong {
    timestamp: u64,
}
