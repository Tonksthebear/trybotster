use std::io;
use std::mem;
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

struct FdGuard(RawFd);

impl FdGuard {
    fn into_stream(self) -> UnixStream {
        let fd = self.0;
        mem::forget(self);
        unsafe { UnixStream::from_raw_fd(fd) }
    }
}

impl Drop for FdGuard {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.0);
        }
    }
}

/// Connect to a Unix socket with a bounded timeout.
///
/// `std::os::unix::net::UnixStream::connect` has no timeout and can block when
/// a peer's accept backlog is saturated. Startup probes use this helper so a
/// stuck existing hub cannot wedge a new hub before signal shutdown is active.
pub(crate) fn connect_with_timeout(path: &Path, timeout: Duration) -> io::Result<UnixStream> {
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = FdGuard(fd);

    let original_flags = unsafe { libc::fcntl(fd.0, libc::F_GETFL) };
    if original_flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd.0, libc::F_SETFL, original_flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }

    let (addr, len) = sockaddr_un(path)?;
    let rc = unsafe {
        libc::connect(
            fd.0,
            &addr as *const libc::sockaddr_un as *const libc::sockaddr,
            len,
        )
    };

    if rc != 0 {
        let err = io::Error::last_os_error();
        if !matches!(
            err.raw_os_error(),
            Some(code) if code == libc::EINPROGRESS || code == libc::EWOULDBLOCK
        ) {
            return Err(err);
        }

        let mut pollfd = libc::pollfd {
            fd: fd.0,
            events: libc::POLLOUT,
            revents: 0,
        };
        let timeout_ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
        let poll_rc = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
        if poll_rc == 0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("timed out connecting to {}", path.display()),
            ));
        }
        if poll_rc < 0 {
            return Err(io::Error::last_os_error());
        }

        let mut socket_error = 0;
        let mut socket_error_len = mem::size_of_val(&socket_error) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                fd.0,
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                &mut socket_error as *mut _ as *mut libc::c_void,
                &mut socket_error_len,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        if socket_error != 0 {
            return Err(io::Error::from_raw_os_error(socket_error));
        }
    }

    if unsafe { libc::fcntl(fd.0, libc::F_SETFL, original_flags) } < 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(fd.into_stream())
}

fn sockaddr_un(path: &Path) -> io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    let bytes = path
        .as_os_str()
        .as_bytes()
        .strip_suffix(&[0])
        .unwrap_or_else(|| path.as_os_str().as_bytes());

    let mut addr: libc::sockaddr_un = unsafe { mem::zeroed() };
    if bytes.len() >= addr.sun_path.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Unix socket path too long: {}", path.display()),
        ));
    }

    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly"
    ))]
    {
        addr.sun_len =
            (mem::size_of::<libc::sockaddr_un>() - addr.sun_path.len() + bytes.len() + 1) as u8;
    }
    for (slot, byte) in addr.sun_path.iter_mut().zip(bytes.iter().copied()) {
        *slot = byte as libc::c_char;
    }

    let len = (mem::size_of::<libc::sockaddr_un>() - addr.sun_path.len() + bytes.len() + 1)
        as libc::socklen_t;

    Ok((addr, len))
}
