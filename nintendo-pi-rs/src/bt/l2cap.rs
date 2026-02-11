//! Async L2CAP socket primitives for raw AF_BLUETOOTH connections.
//!
//! Provides bind, listen, accept, read, and write over L2CAP using
//! raw file descriptors wrapped in tokio's `AsyncFd`.

use std::io;
use std::os::unix::io::{AsRawFd, RawFd};

use tokio::io::unix::AsyncFd;
use tokio::io::Interest;
use tracing::{debug, warn};

// Bluetooth socket constants
const AF_BLUETOOTH: i32 = 31;
const BTPROTO_L2CAP: i32 = 0;
const BDADDR_ANY: [u8; 6] = [0; 6];

/// sockaddr_l2 structure for L2CAP sockets.
#[repr(C)]
struct SockAddrL2 {
    l2_family: u16,
    l2_psm: u16, // little-endian
    l2_bdaddr: [u8; 6],
    l2_cid: u16,
    l2_bdaddr_type: u8,
}

/// Wrapper to impl AsRawFd for a raw fd.
struct RawFdWrapper(RawFd);

impl AsRawFd for RawFdWrapper {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

impl Drop for RawFdWrapper {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.0);
        }
    }
}

/// An async wrapper around a raw L2CAP socket file descriptor.
pub struct L2capSocket {
    inner: AsyncFd<RawFdWrapper>,
}

impl L2capSocket {
    fn from_raw_fd(fd: RawFd) -> io::Result<Self> {
        // Set non-blocking for tokio
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            inner: AsyncFd::new(RawFdWrapper(fd))?,
        })
    }

    pub async fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut guard = self.inner.readable().await?;
            match guard.try_io(|inner| {
                let n = unsafe {
                    libc::recv(inner.as_raw_fd(), buf.as_mut_ptr() as *mut _, buf.len(), 0)
                };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            }) {
                Ok(result) => return result,
                Err(_would_block) => continue,
            }
        }
    }

    pub async fn write_all(&self, data: &[u8]) -> io::Result<()> {
        let mut written = 0;
        while written < data.len() {
            let mut guard = self.inner.writable().await?;
            match guard.try_io(|inner| {
                let n = unsafe {
                    libc::send(
                        inner.as_raw_fd(),
                        data[written..].as_ptr() as *const _,
                        data.len() - written,
                        0,
                    )
                };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            }) {
                Ok(Ok(n)) => written += n,
                Ok(Err(e)) => return Err(e),
                Err(_would_block) => continue,
            }
        }
        Ok(())
    }
}

/// Create, bind, and listen on a raw L2CAP socket for the given PSM.
pub fn bind_and_listen(psm: u16) -> io::Result<RawFd> {
    let fd = unsafe { libc::socket(AF_BLUETOOTH, libc::SOCK_SEQPACKET, BTPROTO_L2CAP) };
    if fd < 0 {
        let err = io::Error::last_os_error();
        warn!("[BT] Failed to create L2CAP socket for PSM {psm}: {err}");
        return Err(err);
    }

    let addr = SockAddrL2 {
        l2_family: AF_BLUETOOTH as u16,
        l2_psm: psm.to_le(),
        l2_bdaddr: BDADDR_ANY,
        l2_cid: 0,
        l2_bdaddr_type: 0, // BREDR
    };

    let ret = unsafe {
        libc::bind(
            fd,
            &addr as *const SockAddrL2 as *const libc::sockaddr,
            std::mem::size_of::<SockAddrL2>() as u32,
        )
    };
    if ret < 0 {
        let err = io::Error::last_os_error();
        unsafe {
            libc::close(fd);
        }
        if err.kind() == io::ErrorKind::AddrInUse {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!(
                    "L2CAP PSM {psm} already in use — \
                     ensure bluetoothd runs with --noplugin=input \
                     (edit /lib/systemd/system/bluetooth.service, add --noplugin=input to ExecStart)"
                ),
            ));
        }
        warn!("[BT] Failed to bind L2CAP socket on PSM {psm}: {err}");
        return Err(err);
    }

    let ret = unsafe { libc::listen(fd, 1) };
    if ret < 0 {
        let err = io::Error::last_os_error();
        unsafe {
            libc::close(fd);
        }
        warn!("[BT] Failed to listen on L2CAP PSM {psm}: {err}");
        return Err(err);
    }

    debug!("[BT] L2CAP listener bound on PSM {psm} (fd={fd})");
    Ok(fd)
}

/// Async accept on a raw listening socket. Returns a connected `L2capSocket`.
///
/// Closes the listener fd after accepting.
pub async fn accept(listener_fd: RawFd) -> io::Result<L2capSocket> {
    let client_fd = async_accept_raw(listener_fd).await?;
    L2capSocket::from_raw_fd(client_fd)
}

/// Async accept that returns the raw fd (closing must be handled by caller).
async fn async_accept_raw(listener_fd: RawFd) -> io::Result<RawFd> {
    // Set listener non-blocking for async accept
    let flags = unsafe { libc::fcntl(listener_fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    unsafe { libc::fcntl(listener_fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };

    let async_fd = AsyncFd::with_interest(RawFdWrapper(listener_fd), Interest::READABLE)?;

    loop {
        let mut guard = async_fd.readable().await?;
        match guard.try_io(|inner| {
            let mut peer_addr: SockAddrL2 = unsafe { std::mem::zeroed() };
            let mut addr_len = std::mem::size_of::<SockAddrL2>() as u32;
            let client_fd = unsafe {
                libc::accept(
                    inner.as_raw_fd(),
                    &mut peer_addr as *mut SockAddrL2 as *mut libc::sockaddr,
                    &mut addr_len,
                )
            };
            if client_fd < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(client_fd)
            }
        }) {
            Ok(result) => {
                // Prevent the AsyncFd from closing the listener fd on drop
                let _ = std::mem::ManuallyDrop::new(async_fd);
                return result;
            }
            Err(_would_block) => continue,
        }
    }
}
