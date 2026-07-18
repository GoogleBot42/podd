//! Runnable demo of the compat API with no hardware.
//!
//! ```sh
//! cargo run -p api --example mock_server
//! # then: curl -s localhost:3000/api/deviceStatus | jq
//! ```

use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let store = Arc::new(api::StateStore::in_memory());
    let control = Arc::new(api::MockControl::new());
    let addr = "127.0.0.1:3000".parse().unwrap();

    println!("mock podd API on http://{addr}/api  (Ctrl-C to stop)");
    api::serve(addr, store, control, None).await
}
