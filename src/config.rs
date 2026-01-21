use std::collections::HashMap;

use serde::Deserialize;
use tokio::fs;

#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    pub listen_addr: String,
    pub root_path: String,
    pub upstreams: HashMap<String, String>,
}

pub async fn load_config(path: &str) -> Result<AppConfig, Box<dyn std::error::Error>> {
    let config_content = fs::read_to_string(path).await?;
    let config: AppConfig = serde_json::from_str(&config_content)?;
    Ok(config)
}
