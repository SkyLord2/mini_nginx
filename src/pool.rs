use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::time::timeout;

#[derive(Clone)]
pub struct ConnectionPool {
    state: Arc<Mutex<PoolState>>,
    max_size: usize,
    max_idle: Duration,
    probe_timeout: Duration,
}

struct PoolState {
    conns: HashMap<String, VecDeque<PooledConn>>,
    total: usize,
}

struct PooledConn {
    stream: TcpStream,
    last_used: Instant,
}

impl ConnectionPool {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(PoolState {
                conns: HashMap::new(),
                total: 0,
            })),
            max_size: 128,
            max_idle: Duration::from_secs(60),
            probe_timeout: Duration::from_millis(200),
        }
    }

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

            if entry.last_used.elapsed() > self.max_idle {
                println!("pool: connection for {} expired", addr);
                continue;
            }

            let mut buf = [0u8; 1];
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

    /// 回收连接：把用完的连接放回池子
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

        while state.total > self.max_size {
            if !evict_oldest(&mut state) {
                break;
            }
        }
    }
}

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
