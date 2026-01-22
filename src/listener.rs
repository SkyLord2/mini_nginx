use tokio::net::{TcpListener, TcpSocket};

/// 创建 TCP 监听器，并按平台设置端口复用
pub fn create_listener(addr: &str) -> Result<TcpListener, Box<dyn std::error::Error>> {
    let socket = TcpSocket::new_v4()?;

    #[cfg(unix)]
    // Unix 下使用 SO_REUSEPORT 允许多进程监听同一端口
    socket.set_reuseport(true)?;
    #[cfg(windows)]
    // Windows 下使用 SO_REUSEADDR
    socket.set_reuseaddr(true)?;

    // 绑定地址并开始监听
    socket.bind(addr.parse()?)?;
    let listener = socket.listen(1024)?;
    Ok(listener)
}
