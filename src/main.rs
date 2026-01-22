mod config;
mod handler;
mod listener;
mod master;
mod mime;
mod worker;
mod pool;

use std::env;

use crate::master::run_master_process;
use crate::worker::run_worker_process;

/// 入口：根据参数决定启动 master 或 worker
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 读取命令行参数
    let args: Vec<String> = env::args().collect();

    // --worker 表示子进程，只负责监听与处理请求
    if args.len() > 1 && args[1] == "--worker" {
        run_worker_process().await?;
    } else {
        // 默认作为 master，负责管理 worker 和热更新
        run_master_process().await?;
    }

    Ok(())
}
