use tokio::net::{TcpListener, TcpSocket};

pub fn create_listener(addr: &str) -> Result<TcpListener, Box<dyn std::error::Error>> {
    let socket = TcpSocket::new_v4()?;

    #[cfg(unix)]
    socket.set_reuseport(true)?;
    #[cfg(windows)]
    socket.set_reuseaddr(true)?;

    socket.bind(addr.parse()?)?;
    let listener = socket.listen(1024)?;
    Ok(listener)
}
