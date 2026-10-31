//! The accept loop the fake servers share.

use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

/// How often a fake server looks for a connection and for its stop.
const POLL: Duration = Duration::from_millis(5);

/// Hands each connection to `serve` on a thread named `name` until `stop` is set. The listener
/// is polled, never left blocked in accept: waking it by connecting to it can fail while the
/// machine is busy, and a server still waiting would hold its test forever.
pub(crate) fn accept_until<F>(listener: &TcpListener, stop: &AtomicBool, name: &str, serve: F)
where
    F: Fn(TcpStream) + Clone + Send + 'static,
{
    if let Err(err) = listener.set_nonblocking(true) {
        eprintln!("{name}: couldn't poll the fake server's port: {err}");
        return;
    }
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            // On macOS an accepted socket takes the listener's non-blocking mode.
            Ok((stream, _)) if stream.set_nonblocking(false).is_ok() => {
                let serve = serve.clone();
                let _ = thread::Builder::new()
                    .name(name.to_owned())
                    .spawn(move || serve(stream));
            }
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => thread::sleep(POLL),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::sync::Arc;
    use std::time::Instant;

    use super::*;

    /// A server stops once its flag is set, with no connection to wake it, and serves every
    /// connection before that.
    #[test]
    fn a_server_stops_on_its_flag_alone() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = stop.clone();
            thread::spawn(move || {
                accept_until(&listener, &stop, "accept-test", |mut stream| {
                    let _ = stream.write_all(b"hello");
                });
            })
        };
        let mut answer = String::new();
        TcpStream::connect(addr)
            .unwrap()
            .read_to_string(&mut answer)
            .unwrap();
        assert_eq!(answer, "hello");
        let asked = Instant::now();
        stop.store(true, Ordering::SeqCst);
        thread.join().unwrap();
        assert!(asked.elapsed() < Duration::from_secs(5));
    }
}
