use std::collections::HashMap;

use serde::Deserialize;
use tokio::fs;

/// 应用配置，包含监听地址、静态根目录、反向代理路由与连接池参数
#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    /// 监听地址，例如 "127.0.0.1:8080"
    pub listen_addr: String,
    /// 静态文件根目录
    pub root_path: String,
    /// 反向代理路由前缀到上游地址的映射
    pub upstreams: HashMap<String, String>,
    /// 连接池配置
    #[serde(default)]
    pub pool: PoolConfig,
}

/// 连接池配置，来自 config.json 的 pool 字段
#[derive(Debug, Deserialize, Clone)]
pub struct PoolConfig {
    /// 连接池最大连接数（全地址总量）
    #[serde(default = "default_pool_max_size")]
    pub max_size: usize,
    /// 连接在池中的最大空闲秒数
    #[serde(default = "default_pool_max_idle_secs")]
    pub max_idle_secs: u64,
    /// 探活超时时间（毫秒）
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

/// 从指定路径读取并解析配置文件
pub async fn load_config(path: &str) -> Result<AppConfig, Box<dyn std::error::Error>> {
    let config_content = fs::read_to_string(path).await?;
    let config: AppConfig = serde_json::from_str(&config_content)?;
    Ok(config)
}
