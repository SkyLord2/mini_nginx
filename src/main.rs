use tokio::net::{TcpListener, TcpStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::fs;

// 1. 处理客户端逻辑现在是 async 的
async fn handle_client(mut stream: TcpStream) {
    let mut buffer = [0; 1024];

    // 2. read 操作变成了异步的，必须加 .await
    match stream.read(&mut buffer).await {
        Ok(size) => {
            if size == 0 { return; } // 连接可能关闭了

            let req_str = String::from_utf8_lossy(&buffer[..size]);
            let first_line = req_str.lines().next().unwrap_or("");
            println!("Request: {:?}", first_line);

            let (status_line, content) = if first_line == "GET / HTTP/1.1" {
                // 3. 读取文件也是异步的，不阻塞线程
                match fs::read_to_string("index.html").await {
                    Ok(content) => ("HTTP/1.1 200 OK", content),
                    Err(_) => ("HTTP/1.1 404 NOT FOUND", String::from("<h1>File not found</h1>")),
                }
            } else {
                ("HTTP/1.1 404 NOT FOUND", String::from("<h1>404 Not Found</h1>"))
            };

            let response = format!(
                "{}\r\nContent-Length: {}\r\n\r\n{}",
                status_line,
                content.len(),
                content
            );

            // 4. write 操作也是异步的
            if let Err(e) = stream.write_all(response.as_bytes()).await {
                eprintln!("Failed to send response: {}", e);
            }
        }
        Err(e) => eprintln!("failed to read from socket: {}", e),
    }
}

// 5. main 函数变成了 async，并使用了 tokio 的宏
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "127.0.0.1:8080";
    let listener = TcpListener::bind(addr).await?;
    println!("Async Server listening on {}", addr);

    loop {
        // 6. accept 也是异步的
        let (stream, _) = listener.accept().await?;

        // 7. 使用 tokio::spawn 而不是 std::thread::spawn
        // 这创建的是一个轻量级的“任务”，而不是系统线程
        tokio::spawn(async move {
            handle_client(stream).await;
        });
    }
}