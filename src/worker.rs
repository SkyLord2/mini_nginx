use std::sync::Arc;

use crate::config::load_config;
use crate::handler::handle_client;
use crate::listener::create_listener;

pub async fn run_worker_process() -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config("config.json").await?;
    let shared_config = Arc::new(config);

    let addr = shared_config.listen_addr.as_str();
    let listener = create_listener(addr)?;

    let id = std::process::id();
    println!("Worker [{}] started on {}", id, addr);

    loop {
        let (stream, _) = listener.accept().await?;
        let config_clone = shared_config.clone();
        tokio::spawn(async move {
            handle_client(stream, config_clone).await;
        });
    }
}
