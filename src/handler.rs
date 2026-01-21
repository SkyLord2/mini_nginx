use std::sync::Arc;

use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::config::AppConfig;
use crate::mime::get_mime_type;
use crate::pool::ConnectionPool;

pub async fn handle_client(mut stream: TcpStream, config: Arc<AppConfig>, pool: ConnectionPool) {
    let mut buffer = [0; 1024];

    let size = match stream.read(&mut buffer).await {
        Ok(n) if n == 0 => return,
        Ok(n) => n,
        Err(_) => return,
    };

    let req_str = String::from_utf8_lossy(&buffer[..size]);
    let first_line = req_str.lines().next().unwrap_or("");
    let path = first_line.split_whitespace().nth(1).unwrap_or("/");

    println!("Request: {} (Path: {})", first_line, path);

    let mut matched_upstream = None;
    for (route, upstream_addr) in &config.upstreams {
        if path.starts_with(route) {
            matched_upstream = Some((route, upstream_addr));
            break;
        }
    }

    if let Some((route, upstream_addr)) = matched_upstream {
        handle_reverse_proxy(&mut stream, &mut buffer, size, upstream_addr, route, pool).await;
    } else {
        handle_static_file(&mut stream, &mut buffer, size, &config.root_path).await;
    }
}

async fn handle_reverse_proxy(
    stream: &mut TcpStream,
    buffer: &mut [u8],
    size: usize,
    upstream_addr: &str,
    route: &str,
    pool: ConnectionPool,
) {
    println!("--> Forwarding to upstream (Port 9000)...");

    match pool.get(upstream_addr).await {
        Ok(mut upstream_stream) => {
            let request_text = String::from_utf8_lossy(&buffer[..size]);
            let new_request_text = request_text.replace(&format!("GET {}", route), "GET /");

            if let Err(e) = upstream_stream.write_all(new_request_text.as_bytes()).await {
                eprintln!("Failed to write to upstream: {}", e);
                return;
            }

            let (mut client_read, mut client_write) = stream.split();
            let (mut upstream_read, mut upstream_write) = upstream_stream.split();

            let client_to_server = tokio::io::copy(&mut client_read, &mut upstream_write);
            let server_to_client = tokio::io::copy(&mut upstream_read, &mut client_write);

            match tokio::join!(client_to_server, server_to_client) {
                (Ok(_), Ok(_)) => {
                    // 只有当 join 正常结束（通常意味着客户端断开了连接，或者 Upstream 关闭了）
                    // 在 Keep-Alive 场景下，如果是客户端先断开，我们就可以回收 Upstream 连接供下一个人用
                    pool.recycle(upstream_addr, upstream_stream);
                }
                (Err(e), _) | (_, Err(e)) => {
                    // 出错了就不回收了，让它自动 Drop 关闭
                    eprintln!("Proxy transfer error: {}", e);
                },
            }
        }
        Err(e) => {
            eprintln!("Failed to connect to upstream: {}", e);
            let _ = stream
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\nUpstream down")
                .await;
        }
    }
}

async fn handle_static_file(stream: &mut TcpStream, buffer: &mut [u8], size: usize, root_path: &str) {
    if size == 0 {
        return;
    }

    let req_str = String::from_utf8_lossy(&buffer[..size]);
    let first_line = req_str.lines().next().unwrap_or("");
    let path = first_line.split_whitespace().nth(1).unwrap_or("/");

    let filename = if path == "/" { "index.html" } else { &path[1..] };
    let file_path = format!("{}/{}", root_path, filename);

    println!("Request: {} -> File: {}", first_line, filename);

    let (status_line, content_type, content) = match fs::read(file_path).await {
        Ok(content) => ("HTTP/1.1 200 OK", get_mime_type(filename), content),
        Err(_) => (
            "HTTP/1.1 404 NOT FOUND",
            "text/html",
            "<h1>404 Not Found</h1>".as_bytes().to_vec(),
        ),
    };

    let header = format!(
        "{}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\r\n",
        status_line,
        content_type,
        content.len()
    );

    if let Err(e) = stream.write_all(header.as_bytes()).await {
        eprintln!("write header error: {}", e);
        return;
    }
    if let Err(e) = stream.write_all(&content).await {
        eprintln!("write body error: {}", e);
    }
}
