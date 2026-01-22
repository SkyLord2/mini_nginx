use std::env;
use std::thread;

use tokio::fs;
use tokio::process::{Child, Command};
use tokio::time::{self, Duration};

/// master 进程：启动 worker 并监听配置文件变化
pub async fn run_master_process() -> Result<(), Box<dyn std::error::Error>> {
    // 优先使用可用 CPU 核心数作为 worker 数量
    let worker_count = thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let self_exe = env::current_exe()?.to_string_lossy().to_string();
    let config_path = "config.json";
    let mut last_modified = fs::metadata(config_path).await?.modified()?;
    let mut workers = spawn_workers(&self_exe, worker_count).await?;

    println!("Master: Running. Modify '{}' to trigger reload.", config_path);

    // 轮询配置文件修改时间，变化则重启 worker
    loop {
        time::sleep(Duration::from_secs(1)).await;
        match fs::metadata(config_path).await {
            Ok(metadata) => {
                if let Ok(modified) = metadata.modified() {
                    if modified > last_modified {
                        println!("\n[!] Config change detected! Reloading...");
                        last_modified = modified;

                        // 先杀掉旧 worker，再拉起新 worker
                        for worker in &mut workers {
                            worker.kill().await?;
                        }
                        match spawn_workers(&self_exe, worker_count).await {
                            Ok(new_workers) => {
                                workers = new_workers;
                                println!("Master: New workers started successfully!");
                            }
                            Err(e) => eprintln!("Master: Failed to spawn workers: {}", e),
                        }
                    }
                }
            }
            Err(err) => {
                eprintln!("Master: Failed to watch config file: {}", err);
            }
        }
    }
}

/// 拉起指定数量的 worker 子进程
async fn spawn_workers(
    exec_path: &str,
    count: usize,
) -> Result<Vec<Child>, Box<dyn std::error::Error>> {
    println!("Master [{}] starting {} workers...", std::process::id(), count);
    let mut children = Vec::new();
    for _ in 0..count {
        let child = Command::new(exec_path)
            .arg("--worker")
            .kill_on_drop(true)
            .spawn()?;
        children.push(child);
    }
    Ok(children)
}
