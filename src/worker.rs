use std::sync::Arc;

use crate::config::load_config;
use crate::handler::handle_client;
use crate::listener::create_listener;
use crate::pool::ConnectionPool;

/// worker 进程：加载配置、初始化连接池并处理请求
pub async fn run_worker_process() -> Result<(), Box<dyn std::error::Error>> {
    // 读取配置并共享给每个连接处理任务
    let config = load_config("config.json").await?;
    let shared_config = Arc::new(config);

    // 初始化连接池，参数来自配置
    let connection_pool = ConnectionPool::new_with_config(&shared_config.pool);

    let addr = shared_config.listen_addr.as_str();
    let listener = create_listener(addr)?;

    let id = std::process::id();
    println!("Worker [{}] started on {}", id, addr);

    // 主循环：接受连接并交给异步任务处理
    loop {
        let (stream, _) = listener.accept().await?;
        let config_clone = shared_config.clone();
        // 克隆连接池句柄（内部为 Arc，成本低）
        let pool_clone = connection_pool.clone();
        tokio::spawn(async move {
            handle_client(stream, config_clone, pool_clone).await;
        });
    }
}
