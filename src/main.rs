use std::net::{ Ipv4Addr, SocketAddrV4, TcpListener};
use std::fs;
use std::io::prelude::*;
fn establish_server(ip: Ipv4Addr, port: u16) {
    let listener = TcpListener::bind(SocketAddrV4::new(ip, port))
    .unwrap_or_else(|err| {
        eprintln!("listen ip address error: {}", err);
        std::process::exit(1);
    });
    
    listener.incoming()
    .filter_map(|stream_rt| {
        stream_rt.map_err(|e| eprintln!("Connection established failed: {}!", e)).ok() 
    })
    .for_each(|mut stream| {
        println!("New connection established!");
        let mut buffer = [0; 1024];

        match stream.read(&mut buffer) {
            Ok(size) => {
                let req_str = String::from_utf8_lossy(&buffer[..size]);
                // 打印第一行请求来看看（方便调试）
                // lines().next() 取出第一行，比如 "GET / HTTP/1.1"
                println!("Request: {:?}", req_str.lines().next().unwrap_or(""));

                // 简单的路由逻辑
                let (status_line, content) = if req_str.starts_with("GET / HTTP/1.1") {
                    // 1. 如果是访问根目录，读取 index.html
                    let content = fs::read_to_string("index.html").unwrap_or_else(|_| String::from("File not found"));
                    ("HTTP/1.1 200 OK", content)
                } else {
                    // 2. 否则返回 404
                    ("HTTP/1.1 404 NOT FOUND", String::from("<h1>404 Not Found</h1>"))
                };

                // 3. 拼接 HTTP 响应
                let response = format!(
                    "{}\r\nContent-Length: {}\r\n\r\n{}",
                    status_line,
                    content.len(),
                    content
                );

                // 4. 发送
                if let Err(e) = stream.write_all(response.as_bytes()) {
                    eprintln!("Failed to send response: {}", e);
                }
            },
            Err(error) => {
                eprintln!("read buffer from stream error: {}", error);
            }
        }
    });
}
fn main() {
    establish_server(Ipv4Addr::new(127, 0, 0, 1), 8080);
}
