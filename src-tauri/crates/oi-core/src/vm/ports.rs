//! Dynamic local port allocation.
//!
//! `OILinux_PRD.md` §6.2: the launcher picks a free port for the SSH `hostfwd` rule and retries
//! with a different one instead of relying on a fixed 2222. Nothing is ever bound to a
//! LAN-reachable address.

use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};

use crate::error::{OiError, OiResult};

/// Ask the OS for a free loopback port, then release it. The caller must be prepared for the
/// small race window: if the port is taken in the meantime, allocation is retried.
pub fn allocate_loopback_port() -> OiResult<u16> {
    for _ in 0..32 {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
            .map_err(|e| OiError::Other(format!("could not allocate a loopback port: {e}")))?;
        let port = listener
            .local_addr()
            .map_err(|e| OiError::Other(format!("could not read the allocated port: {e}")))?
            .port();
        drop(listener);
        if port > 1024 {
            return Ok(port);
        }
    }
    Err(OiError::Other(
        "no usable loopback port could be allocated".into(),
    ))
}

/// Allocate two distinct ports: one for the host SFTP listener, one for the guest side end of the
/// reverse tunnel.
pub fn reserve_pair() -> OiResult<(u16, u16)> {
    let a = allocate_loopback_port()?;
    for _ in 0..32 {
        let b = allocate_loopback_port()?;
        if b != a {
            return Ok((a, b));
        }
    }
    Err(OiError::Other(
        "could not allocate two distinct ports for the share bridge".into(),
    ))
}

/// True when the port is currently free on loopback.
pub fn is_free(port: u16) -> bool {
    TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocated_ports_are_free_and_above_the_reserved_range() {
        let p = allocate_loopback_port().unwrap();
        assert!(p > 1024);
        assert!(is_free(p));
    }

    #[test]
    fn pairs_are_distinct() {
        for _ in 0..8 {
            let (a, b) = reserve_pair().unwrap();
            assert_ne!(a, b);
        }
    }

    #[test]
    fn a_bound_port_reports_as_busy() {
        let l = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(!is_free(port));
    }
}
