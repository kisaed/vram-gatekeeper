//! Free-port allocation for children (spec §9.1 step 2).
//!
//! Responsibility: pick a loopback port nobody is listening on.
//! Not responsible for: retrying when a child fails to bind (the controller's
//! failure backoff covers it).

use std::net::{Ipv4Addr, SocketAddr, TcpListener};

use crate::error::{CoreError, CoreResult};

/// Binds `127.0.0.1:0`, reads the port the OS chose, and releases it.
///
/// There is a small window before the child binds in which another process
/// could take the port; if that happens the child fails to start and the
/// normal start-failure backoff applies (spec §21).
///
/// # Errors
/// Returns [`CoreError::Io`] if the loopback interface cannot be bound.
pub fn free_port() -> CoreResult<u16> {
    let listener =
        TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).map_err(|e| CoreError::io("127.0.0.1:0", e))?;
    let port = listener.local_addr().map_err(|e| CoreError::io("127.0.0.1:0", e))?.port();
    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_a_usable_nonzero_port() {
        let p = free_port().expect("port");
        assert_ne!(p, 0);
        // The port was released, so binding it again must work.
        TcpListener::bind((Ipv4Addr::LOCALHOST, p)).expect("rebind");
    }
}
