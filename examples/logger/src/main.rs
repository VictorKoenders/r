use r::prelude::*;
use std::{
    fs::File,
    io::Write,
    sync::{Arc, Mutex},
};

#[async_std::main]
async fn main() {
    let state = State {
        file: Arc::new(Mutex::new(File::create("log.txt").unwrap())),
    };

    r::System::with_state(state)
        .accept_tcp("0.0.0.0:1234")
        .handle::<Log>()
        .build()
        .await
        .expect("Could not build system")
        .run()
        .await;
}

#[derive(Clone)]
struct State {
    file: Arc<Mutex<File>>,
}

#[derive(serde::Deserialize)]
struct Log {
    pub line: String,
}

#[async_trait]
impl Handler<State> for Log {
    type Error = SerializeAsDebugString<std::io::Error>;
    async fn handle(mut self, state: State, _: &mut HandleContext) -> Result<(), Self::Error> {
        let mut file = state.file.lock().unwrap();
        writeln!(&mut file, "{}", self.line).map_err(SerializeAsDebugString)
    }

    const CAN_HANDLE_STRING: bool = true;
    fn build_from_string(line: String) -> Self {
        Self { line }
    }
}
