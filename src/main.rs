mod config;
mod handler;
mod listener;
mod master;
mod mime;
mod worker;

use std::env;

use crate::master::run_master_process;
use crate::worker::run_worker_process;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    if args.len() > 1 && args[1] == "--worker" {
        run_worker_process().await?;
    } else {
        run_master_process().await?;
    }

    Ok(())
}
