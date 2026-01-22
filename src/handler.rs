use std::sync::Arc;

use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::config::AppConfig;
use crate::mime::get_mime_type;
use crate::pool::ConnectionPool;

/// 处理单个客户端连接：解析请求并分发到静态文件或反向代理
pub async fn handle_client(mut stream: TcpStream, config: Arc<AppConfig>, pool: ConnectionPool) {
    let mut buffer = [0; 1024];

    // 读取首包请求，用于解析请求行
    let size = match stream.read(&mut buffer).await {
        Ok(n) if n == 0 => return,
        Ok(n) => n,
        Err(_) => return,
    };

    let req_str = String::from_utf8_lossy(&buffer[..size]);
    let first_line = req_str.lines().next().unwrap_or("");
    let path = first_line.split_whitespace().nth(1).unwrap_or("/");

    println!("Request: {} (Path: {})", first_line, path);

    // 根据路由前缀匹配上游地址
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

/// 反向代理处理：改写请求行并转发上下游数据
async fn handle_reverse_proxy(
    mut stream: TcpStream,
    buffer: &mut [u8],
    size: usize,
    upstream_addr: &str,
    route: &str,
    pool: ConnectionPool,
) {
    println!("--> Forwarding to upstream {}...", upstream_addr);

    // 从连接池获取上游连接
    match pool.get(upstream_addr).await {
        Ok(mut upstream_stream) => {
            // 改写请求行，把路由前缀转成根路径
            let request_bytes = &buffer[..size];
            let new_request_bytes = rewrite_request_line(request_bytes, route);

            if let Err(e) = upstream_stream.write_all(&new_request_bytes).await {
                eprintln!("Failed to write to upstream: {}", e);
                return;
            }

            // 若存在请求体，继续把剩余请求体转发给上游
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

            // 读取上游响应头，用于判断 keep-alive 与响应体长度
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

            // 根据响应头选择转发方式
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
                    // 仅当上游明确 keep-alive 时才回收连接
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

/// 静态文件处理：根据路径读取文件并构建响应
async fn handle_static_file(stream: &mut TcpStream, buffer: &mut [u8], size: usize, root_path: &str) {
    if size == 0 {
        return;
    }

    let req_str = String::from_utf8_lossy(&buffer[..size]);
    let first_line = req_str.lines().next().unwrap_or("");
    let path = first_line.split_whitespace().nth(1).unwrap_or("/");

    // 将根路径映射到 index.html
    let filename = if path == "/" { "index.html" } else { &path[1..] };
    let file_path = format!("{}/{}", root_path, filename);

    println!("Request: {} -> File: {}", first_line, filename);

    // 文件存在则返回内容，不存在则返回 404
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

/// 解析后的响应元信息，用于决定是否复用连接
struct ResponseInfo {
    keep_alive: bool,
    content_length: Option<usize>,
    chunked: bool,
}

/// 响应头与已读取的响应体前缀
struct ResponseHead {
    header: Vec<u8>,
    body_prefix: Vec<u8>,
    info: ResponseInfo,
}

/// 改写请求行，保留其余请求头和内容不变
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

/// 从请求头解析 Content-Length（若存在）
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

/// 读取上游响应头，返回 header 与已读到的 body 前缀
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

/// 解析响应头中的连接复用与正文长度信息
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

/// 按 Content-Length 转发剩余响应体
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

/// 无明确长度时，读取至 EOF
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

/// 转发 chunked 响应体，直至遇到 0 长度块
async fn relay_chunked(
    upstream: &mut TcpStream, // 上游连接
    client: &mut TcpStream, // 客户端连接
    mut buffer: Vec<u8>, // 已读取的响应体前缀缓冲
) -> Result<(), std::io::Error> { // 返回转发结果
    if !buffer.is_empty() { // 若已有缓存，先转发
        client.write_all(&buffer).await?; // 发送缓存数据
    } // 缓存发送完毕

    let mut parse_pos = 0usize; // 当前解析位置
    let mut temp = [0u8; 4096]; // 读取临时缓冲

    loop { // 循环解析每个 chunk
        let line_end = match buffer[parse_pos..] // 从当前位置查找行结束
            .windows(2) // 以 \r\n 为分隔
            .position(|w| w == b"\r\n") // 找到行结束位置
        { // 匹配结果分支
            Some(pos) => parse_pos + pos, // 计算行结束的绝对位置
            None => { // 缓冲不足，需要继续读取
                let n = upstream.read(&mut temp).await?; // 从上游读取更多数据
                if n == 0 { // 上游提前关闭
                    return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "chunked eof")); // 返回 EOF 错误
                } // 上游未关闭
                buffer.extend_from_slice(&temp[..n]); // 扩展缓冲区
                client.write_all(&temp[..n]).await?; // 同步转发读到的数据
                continue; // 继续尝试解析
            } // 缓冲不足分支结束
        }; // 得到行结束位置

        let size_line = &buffer[parse_pos..line_end]; // 获取 chunk size 行
        let size = parse_chunk_size(size_line)?; // 解析 chunk 长度
        let after_line = line_end + 2; // 跳过 \r\n 的起始位置

        if size == 0 { // 遇到最后一个 chunk
            // 读取可能存在的 trailer 并结束
            loop { // 继续读取直到 trailer 结束
                if let Some(pos) = buffer[after_line..] // 在剩余缓冲中找 trailer 结束
                    .windows(4) // 查找 \r\n\r\n
                    .position(|w| w == b"\r\n\r\n") // 定位 trailer 结束
                { // trailer 结束分支
                    let _ = after_line + pos + 4; // 计算结束位置（仅用于保证逻辑完整）
                    return Ok(()); // 完成 chunked 转发
                } // trailer 未结束
                let n = upstream.read(&mut temp).await?; // 继续从上游读取
                if n == 0 { // 上游提前关闭
                    return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "chunked eof")); // 返回 EOF 错误
                } // 上游未关闭
                buffer.extend_from_slice(&temp[..n]); // 扩展缓冲区
                client.write_all(&temp[..n]).await?; // 同步转发读到的数据
            } // trailer 读取循环结束
        } // size == 0 分支结束

        let needed = after_line + size + 2; // 该 chunk 包含 size + \r\n 的完整长度
        while buffer.len() < needed { // 缓冲区不够则继续读取
            let n = upstream.read(&mut temp).await?; // 从上游读取更多数据
            if n == 0 { // 上游提前关闭
                return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "chunked eof")); // 返回 EOF 错误
            } // 上游未关闭
            buffer.extend_from_slice(&temp[..n]); // 扩展缓冲区
            client.write_all(&temp[..n]).await?; // 同步转发读到的数据
        } // 已达到完整 chunk 长度
        parse_pos = needed; // 移动到下一个 chunk 的起点
    } // 继续处理下一个 chunk
} // relay_chunked 结束

/// 解析 chunk size 的十六进制长度
fn parse_chunk_size(line: &[u8]) -> Result<usize, std::io::Error> {
    let mut end = line.len(); // 默认读取到行尾
    if let Some(pos) = line.iter().position(|b| *b == b';') { // 遇到分号说明有扩展字段
        end = pos; // 截断到分号前
    } // 分号处理结束
    let size_str = String::from_utf8_lossy(&line[..end]); // 解析十六进制字符串
    usize::from_str_radix(size_str.trim(), 16) // 转换为数字
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid chunk size")) // 映射解析错误
} // parse_chunk_size 结束
