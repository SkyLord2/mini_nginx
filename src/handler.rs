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
        handle_reverse_proxy(stream, &mut buffer, size, upstream_addr, route, pool).await;
    } else {
        handle_static_file(&mut stream, &mut buffer, size, &config.root_path).await;
    }
}

async fn handle_reverse_proxy(
    mut stream: TcpStream,
    buffer: &mut [u8],
    size: usize,
    upstream_addr: &str,
    route: &str,
    pool: ConnectionPool,
) {
    println!("--> Forwarding to upstream (Port 9000)...");

    match pool.get(upstream_addr).await {
        Ok(mut upstream_stream) => {
            let request_bytes = &buffer[..size];
            let new_request_bytes = rewrite_request_line(request_bytes, route);

            if let Err(e) = upstream_stream.write_all(&new_request_bytes).await {
                eprintln!("Failed to write to upstream: {}", e);
                return;
            }

            if let Some((header_end, content_length)) = request_content_length(request_bytes) {
                let mut remaining = content_length.saturating_sub(request_bytes.len().saturating_sub(header_end));
                let mut temp = [0u8; 4096];
                while remaining > 0 {
                    let n = match stream.read(&mut temp).await {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(_) => break,
                    };
                    let to_write = n.min(remaining);
                    if upstream_stream.write_all(&temp[..to_write]).await.is_err() {
                        return;
                    }
                    remaining -= to_write;
                }
            }

            let response_head = match read_response_head(&mut upstream_stream).await {
                Ok(head) => head,
                Err(e) => {
                    eprintln!("Failed to read response: {}", e);
                    return;
                }
            };

            if stream.write_all(&response_head.header).await.is_err() {
                return;
            }

            let relay_result = if response_head.info.chunked {
                relay_chunked(&mut upstream_stream, &mut stream, response_head.body_prefix).await
            } else if let Some(content_length) = response_head.info.content_length {
                if !response_head.body_prefix.is_empty() {
                    if stream.write_all(&response_head.body_prefix).await.is_err() {
                        return;
                    }
                }
                relay_content_length(
                    &mut upstream_stream,
                    &mut stream,
                    content_length,
                    response_head.body_prefix.len(),
                )
                .await
            } else {
                if !response_head.body_prefix.is_empty() {
                    if stream.write_all(&response_head.body_prefix).await.is_err() {
                        return;
                    }
                }
                relay_until_eof(&mut upstream_stream, &mut stream).await
            };

            match relay_result {
                Ok(()) => {
                    if response_head.info.keep_alive {
                        pool.recycle(upstream_addr, upstream_stream);
                    }
                }
                Err(e) => {
                    eprintln!("Proxy transfer error: {}", e);
                }
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

struct ResponseInfo {
    keep_alive: bool,
    content_length: Option<usize>,
    chunked: bool,
}

struct ResponseHead {
    header: Vec<u8>,
    body_prefix: Vec<u8>,
    info: ResponseInfo,
}

fn rewrite_request_line(request_bytes: &[u8], route: &str) -> Vec<u8> {
    let line_end = match request_bytes.windows(2).position(|w| w == b"\r\n") {
        Some(end) => end,
        None => return request_bytes.to_vec(),
    };

    let line = String::from_utf8_lossy(&request_bytes[..line_end]);
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");
    let version = parts.next().unwrap_or("");

    if method.is_empty() || version.is_empty() {
        return request_bytes.to_vec();
    }

    let new_path = if path.starts_with(route) {
        path.replacen(route, "/", 1)
    } else {
        path.to_string()
    };

    let new_line = format!("{} {} {}", method, new_path, version);
    let mut out = Vec::with_capacity(request_bytes.len());
    out.extend_from_slice(new_line.as_bytes());
    out.extend_from_slice(&request_bytes[line_end..]);
    out
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

fn request_content_length(request_bytes: &[u8]) -> Option<(usize, usize)> {
    let header_end = find_header_end(request_bytes)?;
    let header_str = String::from_utf8_lossy(&request_bytes[..header_end]);
    for line in header_str.lines() {
        if let Some((key, value)) = line.split_once(':') {
            if key.trim().eq_ignore_ascii_case("content-length") {
                if let Ok(len) = value.trim().parse::<usize>() {
                    return Some((header_end, len));
                }
            }
        }
    }
    None
}

async fn read_response_head(stream: &mut TcpStream) -> Result<ResponseHead, std::io::Error> {
    let mut buffer = Vec::with_capacity(4096);
    let mut temp = [0u8; 4096];

    loop {
        let n = stream.read(&mut temp).await?;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "upstream closed"));
        }
        buffer.extend_from_slice(&temp[..n]);
        if buffer.len() > 32768 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "header too large"));
        }
        if let Some(end) = find_header_end(&buffer) {
            let header = buffer[..end].to_vec();
            let body_prefix = buffer[end..].to_vec();
            let info = parse_response_info(&header);
            return Ok(ResponseHead {
                header,
                body_prefix,
                info,
            });
        }
    }
}

fn parse_response_info(header: &[u8]) -> ResponseInfo {
    let header_str = String::from_utf8_lossy(header);
    let mut lines = header_str.lines();
    let status_line = lines.next().unwrap_or("");
    let is_http10 = status_line.starts_with("HTTP/1.0");
    let mut connection: Option<String> = None;
    let mut content_length: Option<usize> = None;
    let mut chunked = false;

    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim().to_ascii_lowercase();
            match key.as_str() {
                "connection" => connection = Some(value),
                "content-length" => content_length = value.parse::<usize>().ok(),
                "transfer-encoding" => {
                    if value.contains("chunked") {
                        chunked = true;
                    }
                }
                _ => {}
            }
        }
    }

    let keep_alive = if is_http10 {
        connection.as_deref().map(|v| v.contains("keep-alive")).unwrap_or(false)
    } else {
        !connection.as_deref().map(|v| v.contains("close")).unwrap_or(false)
    };

    ResponseInfo {
        keep_alive,
        content_length,
        chunked,
    }
}

async fn relay_content_length(
    upstream: &mut TcpStream,
    client: &mut TcpStream,
    content_length: usize,
    already_sent: usize,
) -> Result<(), std::io::Error> {
    let mut remaining = content_length.saturating_sub(already_sent);
    let mut temp = [0u8; 4096];
    while remaining > 0 {
        let n = upstream.read(&mut temp).await?;
        if n == 0 {
            break;
        }
        let to_write = n.min(remaining);
        client.write_all(&temp[..to_write]).await?;
        remaining -= to_write;
    }
    Ok(())
}

async fn relay_until_eof(upstream: &mut TcpStream, client: &mut TcpStream) -> Result<(), std::io::Error> {
    let mut temp = [0u8; 4096];
    loop {
        let n = upstream.read(&mut temp).await?;
        if n == 0 {
            break;
        }
        client.write_all(&temp[..n]).await?;
    }
    Ok(())
}

async fn relay_chunked(
    upstream: &mut TcpStream,
    client: &mut TcpStream,
    mut buffer: Vec<u8>,
) -> Result<(), std::io::Error> {
    if !buffer.is_empty() {
        client.write_all(&buffer).await?;
    }

    let mut parse_pos = 0usize;
    let mut temp = [0u8; 4096];

    loop {
        let line_end = match buffer[parse_pos..]
            .windows(2)
            .position(|w| w == b"\r\n")
        {
            Some(pos) => parse_pos + pos,
            None => {
                let n = upstream.read(&mut temp).await?;
                if n == 0 {
                    return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "chunked eof"));
                }
                buffer.extend_from_slice(&temp[..n]);
                client.write_all(&temp[..n]).await?;
                continue;
            }
        };

        let size_line = &buffer[parse_pos..line_end];
        let size = parse_chunk_size(size_line)?;
        let after_line = line_end + 2;

        if size == 0 {
            loop {
                if let Some(pos) = buffer[after_line..]
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                {
                    let _ = after_line + pos + 4;
                    return Ok(());
                }
                let n = upstream.read(&mut temp).await?;
                if n == 0 {
                    return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "chunked eof"));
                }
                buffer.extend_from_slice(&temp[..n]);
                client.write_all(&temp[..n]).await?;
            }
        }

        let needed = after_line + size + 2;
        while buffer.len() < needed {
            let n = upstream.read(&mut temp).await?;
            if n == 0 {
                return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "chunked eof"));
            }
            buffer.extend_from_slice(&temp[..n]);
            client.write_all(&temp[..n]).await?;
        }
        parse_pos = needed;
    }
}

fn parse_chunk_size(line: &[u8]) -> Result<usize, std::io::Error> {
    let mut end = line.len();
    if let Some(pos) = line.iter().position(|b| *b == b';') {
        end = pos;
    }
    let size_str = String::from_utf8_lossy(&line[..end]);
    usize::from_str_radix(size_str.trim(), 16)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid chunk size"))
}
