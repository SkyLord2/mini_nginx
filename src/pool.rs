use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::net::TcpStream;

#[derive(Clone)]
pub struct ConnectionPool {
    conns: Arc<Mutex<HashMap<String, Vec<TcpStream>>>>,
}

impl ConnectionPool {
    pub fn new() -> Self {
        Self {
            conns: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn get(&self, addr: &str) -> Result<TcpStream, std::io::Error> {
        loop {
            let stream = {
                let mut pools = self.conns.lock().unwrap();
                pools.get_mut(addr).and_then(|streams| streams.pop())
            };

            let stream = match stream {
                Some(stream) => stream,
                None => break,
            };

            let mut buf = [0u8; 1];
            match stream.peek(&mut buf).await {
                Ok(0) => continue,
                Ok(_) => {
                    println!("pool: reused connection for {}", addr);
                    return Ok(stream);
                }
                Err(_) => continue,
            }
        }

        // 3. 没拿到，建立新连接
        println!("pool: creating new connection for {}", addr);
        TcpStream::connect(addr).await
    }

    /// 回收连接：把用完的连接放回池子
    pub fn recycle(&self, addr: &str, stream: TcpStream) {
        println!("pool: recycling connection for {}", addr);
        let mut pools = self.conns.lock().unwrap();
        pools.entry(addr.to_string()).or_insert_with(Vec::new).push(stream);
    }
}
