use std::sync::Arc;

use crate::config::load_config;
use crate::handler::handle_client;
use crate::listener::create_listener;
use crate::pool::ConnectionPool;

pub async fn run_worker_process() -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config("config.json").await?;
    let shared_config = Arc::new(config);

    // 🔥 初始化连接池
    let connection_pool = ConnectionPool::new();

    let addr = shared_config.listen_addr.as_str();
    let listener = create_listener(addr)?;

    let id = std::process::id();
    println!("Worker [{}] started on {}", id, addr);

    loop {
        let (stream, _) = listener.accept().await?;
        let config_clone = shared_config.clone();
        // 🔥 克隆 pool 引用 (开销极小，因为内部是 Arc)
        let pool_clone = connection_pool.clone();
        tokio::spawn(async move {
            handle_client(stream, config_clone, pool_clone).await;
        });
    }
}
