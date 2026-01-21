use std::env;
use std::process::Command;
use std::collections::HashMap;
use std::sync::Arc;

use tokio::net::{TcpListener, TcpStream, TcpSocket};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::fs;

use serde::Deserialize;

// 定义配置结构体
#[derive(Debug, Deserialize, Clone)]
struct AppConfig {
    listen_addr: String,
    root_path: String,
    // 使用 HashMap 来存储多条反向代理规则
    upstreams: HashMap<String, String>, 
}

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

async fn handle_client(mut stream: TcpStream, config: Arc<AppConfig>) {
    let mut buffer = [0; 1024];

    // 1. 先读一次，看看用户想要什么
    let size = match stream.read(&mut buffer).await {
        Ok(n) if n == 0 => return,
        Ok(n) => n,
        Err(_) => return,
    };

    let req_str = String::from_utf8_lossy(&buffer[..size]);
    let first_line = req_str.lines().next().unwrap_or("");
    let path = first_line.split_whitespace().nth(1).unwrap_or("/");

    println!("Request: {} (Path: {})", first_line, path);

    // 🔥 使用 config.upstreams 动态查找代理规则
    // 检查 path 是否匹配配置中的任何一个 key (例如 "/proxy")
    let mut matched_upstream = None;
    for (route, upstream_addr) in &config.upstreams {
        if path.starts_with(route) {
            matched_upstream = Some((route, upstream_addr));
            break;
        }
    }

    if let Some((route, upstream_addr)) = matched_upstream {
        handle_reverse_proxy(&mut stream, &mut buffer, size, upstream_addr, route).await;
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
) {  
    println!("--> Forwarding to upstream (Port 9000)...");

    // 连接后端服务器 (Upstream)
    match TcpStream::connect(upstream_addr).await {
        Ok(mut upstream_stream) => {
            // 1. 把原始请求转成字符串
            let request_text = String::from_utf8_lossy(&buffer[..size]);
            
            // 2. 进行简单的路径替换：把 "GET /proxy" 替换成 "GET /"
            // 这样 Python 收到的就是访问根目录的请求了
            let new_request_text = request_text.replace(&format!("GET {}", route), "GET /");
            
            // 3. 发送修改后的请求给 Python
            if let Err(e) = upstream_stream.write_all(new_request_text.as_bytes()).await {
                eprintln!("Failed to write to upstream: {}", e);
                return;
            }

            // B. 建立双向管道
            // split() 把一个流拆成“读句柄”和“写句柄”，这样可以同时读写
            let (mut client_read, mut client_write) = stream.split();
            let (mut upstream_read, mut upstream_write) = upstream_stream.split();

            // 管道 1: 客户端剩余数据 -> 后端
            // 管道 2: 后端响应数据 -> 客户端
            // join! 宏让这两个拷贝任务同时进行
            let client_to_server = tokio::io::copy(&mut client_read, &mut upstream_write);
            let server_to_client = tokio::io::copy(&mut upstream_read, &mut client_write);

            match tokio::join!(client_to_server, server_to_client) {
                (Ok(_), Ok(_)) => {},
                (Err(e), _) | (_, Err(e)) => eprintln!("Proxy transfer error: {}", e),
            }
        }
        Err(e) => {
            eprintln!("Failed to connect to upstream: {}", e);
            let _ = stream.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\nUpstream down").await;
        }
    }
}

async fn handle_static_file(stream: &mut TcpStream, buffer: &mut [u8], size: usize, root_path: &str) {
    if size == 0 { return; }

    let req_str = String::from_utf8_lossy(&buffer[..size]);
    let first_line = req_str.lines().next().unwrap_or("");
    // 解析请求路径，例如 "GET /index.html HTTP/1.1" -> "/index.html"
    let path = first_line.split_whitespace().nth(1).unwrap_or("/");
    
    // 安全处理：如果请求 "/", 默认指向 "index.html"
    let filename = if path == "/" { "index.html" } else { &path[1..] }; // 去掉开头的 /
    // 这里可以拼接路径，简单起见我们直接用 string 拼接
    let file_path = format!("{}/{}", root_path, filename);

    println!("Request: {} -> File: {}", first_line, filename);

    // 统一处理文件读取
    let (status_line, content_type, content) = match fs::read(file_path).await {
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
    let config_content = fs::read_to_string("config.json").await?;
    let config: AppConfig = serde_json::from_str(&config_content)?;
    let shared_config = Arc::new(config);
    
    let addr = shared_config.listen_addr.as_str();
    // 使用新的 helper 函数
    let listener = create_listener(addr)?;
    
    // 获取当前进程 ID，方便观察
    let id = std::process::id();
    println!("Worker [{}] started on {}", id, addr);

    loop {
        let (stream, _) = listener.accept().await?;
        let config_clone = shared_config.clone();
        tokio::spawn(async move {
            handle_client(stream, config_clone).await;
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