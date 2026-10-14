//! Carries connections to the proxy into a session's network namespace. A session without
//! network has only its own loopback, so the launcher leaves two small helpers behind: a
//! connector outside, which opens each connection to the proxy, and a relay inside, which
//! listens at the proxy's address and asks the connector over a socket pair for each connection.
//! The connector passes the connected socket back, so it reaches only the proxy addresses it was
//! given, and the relay never touches the network outside. A container gets the same two halves
//! joined by a Unix socket in a shared folder instead. Neither half reads what passes through.

// Only the Linux launcher runs these; macOS builds them for the tests.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::io::{self, IoSlice, IoSliceMut, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nix::sys::socket::{ControlMessage, ControlMessageOwned, MsgFlags, recvmsg, sendmsg};

/// A proxy that takes longer than this to accept counts as down.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Outside the namespace: opens a connection to `targets[i]` for each byte `i` read from `ctl`,
/// and sends the socket back, or nothing when the connection failed. Returns when `ctl` closes.
pub fn serve_connector(ctl: UnixStream, targets: &[SocketAddr]) {
    let mut ctl = ctl;
    let mut index = [0u8; 1];
    while ctl.read_exact(&mut index).is_ok() {
        let stream = targets
            .get(usize::from(index[0]))
            .and_then(|target| TcpStream::connect_timeout(target, CONNECT_TIMEOUT).ok());
        if send_fd(&ctl, stream.as_ref().map(AsRawFd::as_raw_fd)).is_err() {
            return;
        }
    }
}

/// Inside the namespace: accepts on each listener (the one at `listeners[i]` stands for the
/// connector's `targets[i]`) and carries every connection through the connector. Returns when
/// every listener has failed.
pub fn serve_relay(ctl: UnixStream, listeners: Vec<TcpListener>) {
    let ctl = Arc::new(Mutex::new(ctl));
    let accepting: Vec<_> = listeners
        .into_iter()
        .enumerate()
        .filter_map(|(index, listener)| {
            let index = u8::try_from(index).ok()?;
            let ctl = ctl.clone();
            Some(std::thread::spawn(move || {
                for client in listener.incoming() {
                    let Ok(client) = client else { continue };
                    let ctl = ctl.clone();
                    std::thread::spawn(move || carry(client, &ctl, index));
                }
            }))
        })
        .collect();
    for thread in accepting {
        let _ = thread.join();
    }
}

/// One connection: an upstream from the connector, then bytes both ways until each side ends.
fn carry(client: TcpStream, ctl: &Mutex<UnixStream>, index: u8) {
    let upstream = {
        let Ok(mut ctl) = ctl.lock() else { return };
        if ctl.write_all(&[index]).is_err() {
            return;
        }
        match recv_fd(&ctl) {
            Ok(Some(fd)) => TcpStream::from(fd),
            _ => return,
        }
    };
    splice(client, upstream);
}

/// A stream that can end its sending half and be split in two.
pub trait Duplex: Read + Write + Send + Sized + 'static {
    fn split(&self) -> io::Result<Self>;
    fn end_writes(&self);
}

impl Duplex for TcpStream {
    fn split(&self) -> io::Result<Self> {
        self.try_clone()
    }

    fn end_writes(&self) {
        let _ = self.shutdown(Shutdown::Write);
    }
}

impl Duplex for UnixStream {
    fn split(&self) -> io::Result<Self> {
        self.try_clone()
    }

    fn end_writes(&self) {
        let _ = self.shutdown(Shutdown::Write);
    }
}

fn pipe<A: Duplex, B: Duplex>(mut from: A, mut to: B) {
    let _ = io::copy(&mut from, &mut to);
    to.end_writes();
}

/// Bytes both ways between two streams until each side ends.
fn splice<A: Duplex, B: Duplex>(a: A, b: B) {
    let (Ok(a_back), Ok(b_back)) = (a.split(), b.split()) else {
        return;
    };
    let forward = std::thread::spawn(move || pipe(a, b));
    pipe(b_back, a_back);
    let _ = forward.join();
}

/// Outside a container: each connection to the socket `listener` goes on to `target`.
pub fn serve_unix_to_tcp(listener: UnixListener, target: SocketAddr) {
    for client in listener.incoming() {
        let Ok(client) = client else { continue };
        std::thread::spawn(move || {
            if let Ok(upstream) = TcpStream::connect_timeout(&target, CONNECT_TIMEOUT) {
                splice(client, upstream);
            }
        });
    }
}

/// Inside a container: each connection to `listener` goes on to the socket at `socket`.
pub fn serve_tcp_to_unix(listener: TcpListener, socket: PathBuf) {
    for client in listener.incoming() {
        let Ok(client) = client else { continue };
        let socket = socket.clone();
        std::thread::spawn(move || {
            if let Ok(upstream) = UnixStream::connect(&socket) {
                splice(client, upstream);
            }
        });
    }
}

fn send_fd(ctl: &UnixStream, fd: Option<RawFd>) -> io::Result<()> {
    let flag = [u8::from(fd.is_some())];
    let iov = [IoSlice::new(&flag)];
    let fds: Vec<RawFd> = fd.into_iter().collect();
    let cmsgs: Vec<ControlMessage<'_>> = if fds.is_empty() {
        Vec::new()
    } else {
        vec![ControlMessage::ScmRights(&fds)]
    };
    sendmsg::<()>(ctl.as_raw_fd(), &iov, &cmsgs, MsgFlags::empty(), None)?;
    Ok(())
}

fn recv_fd(ctl: &UnixStream) -> io::Result<Option<OwnedFd>> {
    let mut flag = [0u8; 1];
    let mut iov = [IoSliceMut::new(&mut flag)];
    let mut space = nix::cmsg_space!([RawFd; 1]);
    let msg = recvmsg::<()>(
        ctl.as_raw_fd(),
        &mut iov,
        Some(&mut space),
        MsgFlags::empty(),
    )?;
    if msg.bytes == 0 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    let mut received = None;
    for cmsg in msg.cmsgs()? {
        if let ControlMessageOwned::ScmRights(fds) = cmsg {
            for fd in fds {
                // SAFETY: the kernel just gave this process the descriptor, and nothing else
                // holds it.
                let owned = unsafe { OwnedFd::from_raw_fd(fd) };
                received.get_or_insert(owned);
            }
        }
    }
    Ok(received)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};

    use super::*;

    /// A fake proxy that answers each line with `<name>: <line>`.
    fn echo(name: &'static str) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                std::thread::spawn(move || {
                    let mut writer = stream.try_clone().unwrap();
                    for line in BufReader::new(stream).lines().map_while(Result::ok) {
                        if writeln!(writer, "{name}: {line}").is_err() {
                            return;
                        }
                    }
                });
            }
        });
        addr
    }

    fn ask(addr: SocketAddr, line: &str) -> String {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        writeln!(stream, "{line}").unwrap();
        let mut answer = String::new();
        BufReader::new(stream).read_line(&mut answer).unwrap();
        answer.trim().to_owned()
    }

    // macOS collects sockets in flight between threads of one process while other Unix sockets
    // close, and can flush one it is passing. The launcher passes them only on Linux.
    #[cfg(target_os = "linux")]
    #[test]
    fn each_relay_port_reaches_its_own_proxy() {
        let targets = vec![echo("first"), echo("second")];
        let (inside, outside) = UnixStream::pair().unwrap();
        let connector_targets = targets.clone();
        std::thread::spawn(move || serve_connector(outside, &connector_targets));
        let listeners: Vec<TcpListener> = (0..2)
            .map(|_| TcpListener::bind("127.0.0.1:0").unwrap())
            .collect();
        let relay: Vec<SocketAddr> = listeners
            .iter()
            .map(|listener| listener.local_addr().unwrap())
            .collect();
        std::thread::spawn(move || serve_relay(inside, listeners));

        assert_eq!(ask(relay[0], "hello"), "first: hello");
        assert_eq!(ask(relay[1], "hello"), "second: hello");
        let together: Vec<_> = (0..8)
            .map(|n| {
                let addr = relay[n % 2];
                std::thread::spawn(move || ask(addr, &format!("n{n}")))
            })
            .collect();
        for (n, answer) in together.into_iter().enumerate() {
            let name = if n % 2 == 0 { "first" } else { "second" };
            assert_eq!(answer.join().unwrap(), format!("{name}: n{n}"));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_proxy_that_is_down_closes_the_relayed_connection() {
        let closed = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = closed.local_addr().unwrap();
        drop(closed);
        let (inside, outside) = UnixStream::pair().unwrap();
        std::thread::spawn(move || serve_connector(outside, &[target]));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let relay = listener.local_addr().unwrap();
        std::thread::spawn(move || serve_relay(inside, vec![listener]));

        let mut stream = TcpStream::connect(relay).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut buf = [0u8; 1];
        assert_eq!(stream.read(&mut buf).unwrap(), 0);
    }

    #[test]
    fn a_container_reaches_the_proxy_through_the_bridge_and_relay() {
        let target = echo("proxy");
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("p0");
        let bridge = UnixListener::bind(&socket).unwrap();
        std::thread::spawn(move || serve_unix_to_tcp(bridge, target));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let relay = listener.local_addr().unwrap();
        std::thread::spawn(move || serve_tcp_to_unix(listener, socket));
        assert_eq!(ask(relay, "through"), "proxy: through");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_connector_only_dials_the_targets_it_was_given() {
        let target = echo("only");
        let (mut inside, outside) = UnixStream::pair().unwrap();
        std::thread::spawn(move || serve_connector(outside, &[target]));
        inside.write_all(&[7]).unwrap();
        assert!(recv_fd(&inside).unwrap().is_none());
        inside.write_all(&[0]).unwrap();
        assert!(recv_fd(&inside).unwrap().is_some());
    }
}
