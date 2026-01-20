use std::env;
use std::process::Command;

use tokio::net::{TcpListener, TcpStream, TcpSocket};
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

fn create_listener(addr: &str) -> Result<TcpListener, Box<dyn std::error::Error>> {
    let socket = TcpSocket::new_v4()?;

    #[cfg(unix)]
    socket.set_reuseport(true)?;
    #[cfg(windows)]
    socket.set_reuseaddr(true)?;
    
    socket.bind(addr.parse()?)?;
    let listener = socket.listen(1024)?;
    Ok(listener)
}

// 👷 Worker 逻辑：这就是我们之前写的服务器主循环
async fn run_worker_process() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "127.0.0.1:8080";
    // 使用新的 helper 函数
    let listener = create_listener(addr)?;
    
    // 获取当前进程 ID，方便观察
    let id = std::process::id();
    println!("Worker [{}] started on {}", id, addr);

    loop {
        let (stream, _) = listener.accept().await?;
        tokio::spawn(async move {
            handle_client(stream).await;
        });
    }
}

// 🤵 Master 逻辑：只负责管理
fn run_master_process() -> Result<(), Box<dyn std::error::Error>> {
    // 假设我们要启动 4 个 Worker (通常等于 CPU 核心数)
    let worker_count = 4;
    println!("Master [{}] starting {} workers...", std::process::id(), worker_count);

    let mut children = Vec::new();

    // 获取当前程序自己的路径，为了能在子进程里再次启动自己
    let self_exe = env::current_exe()?;

    for _ in 0..worker_count {
        // 启动子进程，并传入 "--worker" 参数
        let child = Command::new(&self_exe)
            .arg("--worker")
            .spawn()?;
        children.push(child);
    }

    // Master 进入等待状态，防止主进程退出
    // 在真正的 Nginx 里，这里会监控信号和子进程状态
    for mut child in children {
        child.wait()?;
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 获取命令行参数
    let args: Vec<String> = env::args().collect();

    // 简单的参数路由
    if args.len() > 1 && args[1] == "--worker" {
        // 如果有 --worker 参数，就当员工
        run_worker_process().await?;
    } else {
        // 否则就当老板
        run_master_process()?;
    }

    Ok(())
}