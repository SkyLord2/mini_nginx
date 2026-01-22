use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::config::PoolConfig;

/// 上游连接池：按地址分组，提供 LRU 回收与探活能力
#[derive(Clone)]
pub struct ConnectionPool {
    /// 共享状态（连接列表与总量）
    state: Arc<Mutex<PoolState>>,
    /// 连接池最大连接数（全地址总量）
    max_size: usize,
    /// 允许的最大空闲时长
    max_idle: Duration,
    /// 探活超时（peek 超时）
    probe_timeout: Duration,
}

/// 连接池内部状态
struct PoolState {
    /// 每个地址对应的连接队列（队头最老，队尾最新）
    conns: HashMap<String, VecDeque<PooledConn>>,
    /// 当前池中连接总数
    total: usize,
}

/// 池内连接及其最近使用时间
struct PooledConn {
    stream: TcpStream,
    last_used: Instant,
}

impl ConnectionPool {
    /// 基于配置初始化连接池
    pub fn new_with_config(config: &PoolConfig) -> Self {
        Self {
            state: Arc::new(Mutex::new(PoolState {
                conns: HashMap::new(),
                total: 0,
            })),
            max_size: config.max_size,
            max_idle: Duration::from_secs(config.max_idle_secs),
            probe_timeout: Duration::from_millis(config.probe_timeout_ms),
        }
    }

    /// 获取可用连接：优先复用池内连接，否则新建
    pub async fn get(&self, addr: &str) -> Result<TcpStream, std::io::Error> {
        loop {
            let entry = {
                let mut state = self.state.lock().unwrap();
                let entry = state
                    .conns
                    .get_mut(addr)
                    .and_then(|streams| streams.pop_back());
                if entry.is_some() {
                    state.total = state.total.saturating_sub(1);
                }
                entry
            };

            let entry = match entry {
                Some(entry) => entry,
                None => {
                    println!("pool: no connection for {}", addr);
                    break
                },
            };

            // 超过最大空闲时间则丢弃
            if entry.last_used.elapsed() > self.max_idle {
                println!("pool: connection for {} expired", addr);
                continue;
            }

            let mut buf = [0u8; 1];
            // 探活：在超时内 peek，判断是否仍可用
            match timeout(self.probe_timeout, entry.stream.peek(&mut buf)).await {
                Ok(Ok(0)) => {
                    println!("pool: connection for {} is closed", addr);
                    continue
                },
                Ok(Ok(_)) => {
                    println!("pool: reused connection for {}", addr);
                    return Ok(entry.stream);
                }
                Ok(Err(_)) | Err(_) => {
                    println!("pool: connection for {} is closed", addr);
                    continue
                },
            }
        }

        // 3. 没拿到，建立新连接
        println!("pool: creating new connection for {}", addr);
        TcpStream::connect(addr).await
    }

    /// 回收连接：把用完的连接放回池子，并触发 LRU 淘汰
    pub fn recycle(&self, addr: &str, stream: TcpStream) {
        println!("pool: recycling connection for {}", addr);
        let mut state = self.state.lock().unwrap();
        state
            .conns
            .entry(addr.to_string())
            .or_insert_with(VecDeque::new)
            .push_back(PooledConn {
                stream,
                last_used: Instant::now(),
            });
        state.total = state.total.saturating_add(1);

        // 超出最大容量时，按全局最旧连接淘汰
        while state.total > self.max_size {
            if !evict_oldest(&mut state) {
                break;
            }
        }
    }
}

/// 淘汰全局最旧连接
fn evict_oldest(state: &mut PoolState) -> bool {
    let mut oldest_addr: Option<String> = None;
    let mut oldest_time: Option<Instant> = None;

    for (addr, list) in state.conns.iter() {
        if let Some(front) = list.front() {
            if oldest_time.map_or(true, |t| front.last_used < t) {
                oldest_time = Some(front.last_used);
                oldest_addr = Some(addr.clone());
            }
        }
    }

    let addr = match oldest_addr {
        Some(addr) => addr,
        None => return false,
    };

    if let Some(list) = state.conns.get_mut(&addr) {
        if list.pop_front().is_some() {
            state.total = state.total.saturating_sub(1);
        }
        if list.is_empty() {
            state.conns.remove(&addr);
        }
    }

    true
}
