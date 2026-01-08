use tokio::net::{TcpListener, TcpStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::fs;

// 辅助函数：根据文件名获取 MIME 类型 (Content-Type)
// 这是一个简单的 match，以后可以用 crate 替代
fn get_mime_type(filename: &str) -> &str {
    if filename.ends_with(".html") { "text/html" }
    else if filename.ends_with(".css") { "text/css" }
    else if filename.ends_with(".js") { "application/javascript" }
    else if filename.ends_with(".png") { "image/png" }
    else if filename.ends_with(".jpg") || filename.ends_with(".jpeg") { "image/jpeg" }
    else { "application/octet-stream" } // 默认二进制流
}

async fn handle_client(mut stream: TcpStream) {
    let mut buffer = [0; 1024];

    match stream.read(&mut buffer).await {
        Ok(size) => {
            if size == 0 { return; }

            let req_str = String::from_utf8_lossy(&buffer[..size]);
            let first_line = req_str.lines().next().unwrap_or("");
            // 解析请求路径，例如 "GET /index.html HTTP/1.1" -> "/index.html"
            let path = first_line.split_whitespace().nth(1).unwrap_or("/");
            
            // 安全处理：如果请求 "/", 默认指向 "index.html"
            let filename = if path == "/" { "index.html" } else { &path[1..] }; // 去掉开头的 /

            println!("Request: {} -> File: {}", first_line, filename);

            // 统一处理文件读取
            let (status_line, content_type, content) = match fs::read(filename).await {
                Ok(content) => {
                    ("HTTP/1.1 200 OK", get_mime_type(filename), content)
                }
                Err(_) => {
                    // 404 时返回一段简单的 HTML 字节
                    ("HTTP/1.1 404 NOT FOUND", "text/html", "<h1>404 Not Found</h1>".as_bytes().to_vec())
                }
            };

            // 组装响应头
            let header = format!(
                "{}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\r\n",
                status_line, content_type, content.len()
            );

            // 发送头部
            if let Err(e) = stream.write_all(header.as_bytes()).await {
                eprintln!("write header error: {}", e);
                return;
            }
            // 发送内容（可能是图片二进制数据，也可能是文本）
            if let Err(e) = stream.write_all(&content).await {
                eprintln!("write body error: {}", e);
            }
        }
        Err(e) => eprintln!("failed to read from socket: {}", e),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "127.0.0.1:8080";
    let listener = TcpListener::bind(addr).await?;
    println!("Async Server listening on {}", addr);

    loop {
        let (stream, _) = listener.accept().await?;
        tokio::spawn(async move {
            handle_client(stream).await;
        });
    }
}