use std::collections::HashMap;

use serde::Deserialize;
use tokio::fs;

#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    pub listen_addr: String,
    pub root_path: String,
    pub upstreams: HashMap<String, String>,
    #[serde(default)]
    pub pool: PoolConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PoolConfig {
    #[serde(default = "default_pool_max_size")]
    pub max_size: usize,
    #[serde(default = "default_pool_max_idle_secs")]
    pub max_idle_secs: u64,
    #[serde(default = "default_pool_probe_timeout_ms")]
    pub probe_timeout_ms: u64,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_size: default_pool_max_size(),
            max_idle_secs: default_pool_max_idle_secs(),
            probe_timeout_ms: default_pool_probe_timeout_ms(),
        }
    }
}

fn default_pool_max_size() -> usize {
    128
}

fn default_pool_max_idle_secs() -> u64 {
    60
}

fn default_pool_probe_timeout_ms() -> u64 {
    200
}

pub async fn load_config(path: &str) -> Result<AppConfig, Box<dyn std::error::Error>> {
    let config_content = fs::read_to_string(path).await?;
    let config: AppConfig = serde_json::from_str(&config_content)?;
    Ok(config)
}
