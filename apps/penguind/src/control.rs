//! The control socket: line-delimited JSON requests ([`wpe_ipc::Request`])
//! from any number of clients, served from the event loop. Each client's
//! responses come in the order of its requests; a request answered later
//! (a wallpaper loading) holds back that client's next ones, not others'.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::ping::{Ping, make_ping};
use smithay_client_toolkit::reexports::calloop::{Interest, LoopHandle, Mode, PostAction};
use wpe_ipc::{Request, Response};

/// Requests longer than this are refused (and the client dropped).
const MAX_LINE: usize = 1 << 20;
/// A client that doesn't read its responses for this long is dropped.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// Whoever answers requests: the daemon. Each request comes with the
/// [`Responder`] that answers it, at once or later (loading a wallpaper);
/// until it has, the client's further requests wait, so responses stay in
/// request order.
pub trait Handler {
    fn handle_request(&mut self, request: Request, responder: Responder);
}

/// One connected client: the response side of its stream, and the request
/// bytes not handled yet.
struct ClientState {
    stream: UnixStream,
    pending: VecDeque<u8>,
    /// A request's response is outstanding.
    busy: bool,
    /// The stream failed.
    closed: bool,
    /// The client sent all it will.
    eof: bool,
    /// Wakes the loop to handle requests that waited on a response.
    wake: Ping,
}

impl ClientState {
    fn write(&mut self, response: &Response) {
        if self.closed {
            return;
        }
        let line = wpe_ipc::encode_line(response);
        if (&self.stream).write_all(line.as_bytes()).is_err() {
            self.closed = true;
        }
    }
}

/// Answers one request. Dropping it unused answers with an error.
pub struct Responder(Option<Rc<RefCell<ClientState>>>);

impl Responder {
    pub fn send(mut self, response: Response) {
        if let Some(client) = self.0.take() {
            let mut client = client.borrow_mut();
            client.write(&response);
            client.busy = false;
            if client.eof || client.pending.contains(&b'\n') {
                client.wake.ping();
            }
        }
    }

    /// Answer `ok` and hand the connection over for events: this side stops
    /// reading requests from it. None when the client is gone already.
    pub fn into_subscriber(mut self) -> Option<UnixStream> {
        let client = self.0.take()?;
        let mut c = client.borrow_mut();
        c.write(&Response::ok());
        let stream = if c.closed {
            None
        } else {
            c.stream.try_clone().ok()
        };
        c.closed = true;
        c.busy = false;
        // Lets the wake source see it's done and go.
        c.wake.ping();
        stream
    }
}

impl Drop for Responder {
    fn drop(&mut self) {
        if self.0.is_some() {
            let responder = Responder(self.0.take());
            responder.send(Response::error("the request was dropped"));
        }
    }
}

/// The listening socket, and the path to remove when we bound it ourselves.
pub struct ControlSocket {
    listener: UnixListener,
    /// None when systemd passed the socket in (it owns the path then).
    pub bound: Option<PathBuf>,
}

impl Drop for ControlSocket {
    fn drop(&mut self) {
        if let Some(path) = &self.bound {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// The socket systemd passed for socket activation, if any.
fn activated() -> Result<Option<UnixListener>> {
    let fds: Vec<RawFd> = sd_notify::listen_fds().context("LISTEN_FDS")?.collect();
    // Don't hand the variables on to children (libmpv, ...).
    // SAFETY: called at startup, before other threads look at the environment.
    unsafe {
        std::env::remove_var("LISTEN_FDS");
        std::env::remove_var("LISTEN_PID");
        std::env::remove_var("LISTEN_FDNAMES");
    }
    let Some(&fd) = fds.first() else {
        return Ok(None);
    };
    if fds.len() > 1 {
        tracing::warn!("systemd passed {} sockets; using the first", fds.len());
    }
    // SAFETY: systemd hands us this descriptor for us alone.
    let listener = unsafe { UnixListener::from_raw_fd(fd) };
    Ok(Some(listener))
}

/// Bind `path`, replacing a stale socket file nobody listens on, but not
/// one another daemon is serving.
fn bind(path: &Path) -> Result<UnixListener> {
    if path.exists() {
        match UnixStream::connect(path) {
            Ok(_) => bail!("another penguind is listening on {}", path.display()),
            Err(_) => {
                tracing::info!("removing stale socket {}", path.display());
                std::fs::remove_file(path)
                    .with_context(|| format!("removing {}", path.display()))?;
            }
        }
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))
}

/// The socket systemd passed in, or else `path` bound here.
pub fn listen(path: Option<&Path>) -> Result<ControlSocket> {
    let socket = match activated()? {
        Some(listener) => {
            tracing::info!("control socket passed in by systemd");
            ControlSocket {
                listener,
                bound: None,
            }
        }
        None => {
            let path = path.context("XDG_RUNTIME_DIR is not set; no control socket path")?;
            let listener = bind(path)?;
            tracing::info!("control socket {}", path.display());
            ControlSocket {
                listener,
                bound: Some(path.to_owned()),
            }
        }
    };
    socket.listener.set_nonblocking(true)?;
    Ok(socket)
}

/// `path` bound here (for tests: no systemd socket).
#[cfg(test)]
pub fn listen_at(path: &Path) -> Result<ControlSocket> {
    let listener = bind(path)?;
    listener.set_nonblocking(true)?;
    Ok(ControlSocket {
        listener,
        bound: Some(path.to_owned()),
    })
}

/// Serve `socket` from the event loop of `handle`. The listener is moved
/// into the loop; the returned guard removes the socket file when dropped.
pub fn serve<D: Handler + 'static>(
    socket: ControlSocket,
    handle: &LoopHandle<'static, D>,
) -> Result<SocketGuard> {
    let listener = socket.listener.try_clone()?;
    let guard = SocketGuard(socket);
    let clients = handle.clone();
    handle
        .insert_source(
            Generic::new(listener, Interest::READ, Mode::Level),
            move |_, listener, _| {
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if let Err(e) = add_client(&clients, stream) {
                                tracing::warn!("control client: {e}");
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) => {
                            tracing::warn!("control socket: {e}");
                            break;
                        }
                    }
                }
                Ok(PostAction::Continue)
            },
        )
        .map_err(|e| anyhow::anyhow!("cannot watch the control socket: {}", e.error))?;
    Ok(guard)
}

/// Keeps the socket (file) until dropped.
pub struct SocketGuard(#[allow(dead_code)] ControlSocket);

fn add_client<D: Handler + 'static>(
    handle: &LoopHandle<'static, D>,
    stream: UnixStream,
) -> Result<()> {
    // Reads happen only when the loop says the socket is readable; writes
    // block for at most the timeout.
    stream.set_nonblocking(false)?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let (wake, wake_source) = make_ping()?;
    let client = Rc::new(RefCell::new(ClientState {
        stream: stream.try_clone()?,
        pending: VecDeque::new(),
        busy: false,
        closed: false,
        eof: false,
        wake,
    }));
    // The wake source outlives the stream's when the client stops sending
    // while a response is outstanding; it goes once that is answered.
    let woken = client.clone();
    let token = Rc::new(Cell::new(None));
    let own_token = token.clone();
    let loop_handle = handle.clone();
    let wake_token = handle
        .insert_source(wake_source, move |_, _, handler| {
            handle_lines(handler, &woken);
            let done = {
                let c = woken.borrow();
                c.closed || (c.eof && !c.busy)
            };
            if done && let Some(token) = own_token.take() {
                let h = loop_handle.clone();
                loop_handle.insert_idle(move |_| h.remove(token));
            }
        })
        .map_err(|e| anyhow::anyhow!("cannot watch a control client: {}", e.error))?;
    token.set(Some(wake_token));
    let inserted = handle.insert_source(
        Generic::new(stream, Interest::READ, Mode::Level),
        move |_, stream, handler| {
            let mut chunk = [0u8; 8192];
            let n = match (&**stream).read(&mut chunk) {
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                    return Ok(PostAction::Continue);
                }
                Err(_) => 0,
            };
            {
                let mut c = client.borrow_mut();
                c.pending.extend(&chunk[..n]);
                if c.pending.len() > MAX_LINE {
                    c.write(&Response::error("request too long"));
                    c.closed = true;
                }
            }
            handle_lines(handler, &client);
            let mut c = client.borrow_mut();
            if n == 0 || c.closed {
                // (A subscriber's events now go through `events`.)
                // Let the wake source finish what is outstanding, then go.
                c.eof = true;
                c.wake.ping();
                return Ok(PostAction::Remove);
            }
            Ok(PostAction::Continue)
        },
    );
    if let Err(e) = inserted {
        if let Some(token) = token.take() {
            handle.remove(token);
        }
        bail!("cannot watch a control client: {}", e.error);
    }
    Ok(())
}

/// Handle the client's complete request lines, until one is waiting on its
/// response.
fn handle_lines<D: Handler>(handler: &mut D, client: &Rc<RefCell<ClientState>>) {
    loop {
        let line = {
            let mut c = client.borrow_mut();
            if c.busy || c.closed {
                return;
            }
            let Some(end) = c.pending.iter().position(|&b| b == b'\n') else {
                return;
            };
            let mut line: Vec<u8> = c.pending.drain(..=end).collect();
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            if c.pending.is_empty() && c.pending.capacity() > 64 * 1024 {
                c.pending.shrink_to(4096);
            }
            line
        };
        match parse(&line) {
            None => {}
            Some(Err(response)) => client.borrow_mut().write(&response),
            Some(Ok(request)) => {
                tracing::debug!("control request: {request:?}");
                client.borrow_mut().busy = true;
                handler.handle_request(request, Responder(Some(client.clone())));
            }
        }
    }
}

/// The request on one line; none for a blank line, or the error response
/// to a malformed one.
fn parse(line: &[u8]) -> Option<Result<Request, Response>> {
    let line = std::str::from_utf8(line).ok().map(str::trim);
    match line {
        Some("") => None,
        Some(line) => Some(
            serde_json::from_str::<Request>(line)
                .map_err(|e| Response::error(format!("bad request: {e}"))),
        ),
        None => Some(Err(Response::error("bad request: not UTF-8"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay_client_toolkit::reexports::calloop::EventLoop;
    use std::io::{BufRead, BufReader};
    use wpe_ipc::Client;

    #[derive(Default)]
    struct Fake {
        requests: Vec<Request>,
        quit: bool,
        /// `set` is answered when a `resume` comes, as a load would.
        deferred: Vec<Responder>,
    }

    impl Handler for Fake {
        fn handle_request(&mut self, request: Request, responder: Responder) {
            self.requests.push(request.clone());
            match request {
                Request::Quit => {
                    self.quit = true;
                    responder.send(Response::ok());
                }
                Request::Fps { fps } if fps > 1000 => responder.send(Response::error("too fast")),
                Request::Set { .. } => self.deferred.push(responder),
                Request::Resume { output: None } => {
                    for r in self.deferred.drain(..) {
                        r.send(Response::error("loaded later"));
                    }
                    responder.send(Response::ok());
                }
                _ => responder.send(Response::ok()),
            }
        }
    }

    fn temp_socket(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wpe-control-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("test.sock")
    }

    #[test]
    fn serves_several_clients() {
        let path = temp_socket("serve");
        let mut event_loop: EventLoop<'static, Fake> = EventLoop::try_new().unwrap();
        let socket = ControlSocket {
            listener: bind(&path).unwrap(),
            bound: Some(path.clone()),
        };
        socket.listener.set_nonblocking(true).unwrap();
        let guard = serve(socket, &event_loop.handle()).unwrap();

        // A second daemon may not take over a live socket.
        assert!(bind(&path).is_err());

        let client_path = path.clone();
        let clients = std::thread::spawn(move || {
            let mut a = Client::connect(&client_path).unwrap();
            let mut b = Client::connect(&client_path).unwrap();
            assert!(a.request(&Request::Status).unwrap().ok);
            let slow = b.request(&Request::Fps { fps: 5000 }).unwrap();
            assert_eq!(slow.error.as_deref(), Some("too fast"));

            // Raw lines: two requests in one write, garbage, a blank line.
            let mut raw = UnixStream::connect(&client_path).unwrap();
            raw.write_all(
                b"{\"cmd\":\"pause\"}\n\n{\"cmd\":\"resume\",\"output\":\"DP-1\"}\nnope\n",
            )
            .unwrap();
            let mut reader = BufReader::new(raw.try_clone().unwrap());
            let mut lines = Vec::new();
            for _ in 0..3 {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                lines.push(serde_json::from_str::<Response>(&line).unwrap());
            }
            assert!(lines[0].ok && lines[1].ok);
            assert!(
                lines[2]
                    .error
                    .as_deref()
                    .unwrap()
                    .starts_with("bad request")
            );
            drop(raw);

            // A `set` answered later holds back its client's next request,
            // but not other clients'.
            let mut raw = UnixStream::connect(&client_path).unwrap();
            raw.write_all(b"{\"cmd\":\"set\",\"wallpaper\":\"x\"}\n{\"cmd\":\"status\"}\n")
                .unwrap();
            std::thread::sleep(Duration::from_millis(100));
            assert!(b.request(&Request::Status).unwrap().ok);
            assert!(b.request(&Request::Resume { output: None }).unwrap().ok);
            let mut reader = BufReader::new(raw.try_clone().unwrap());
            let mut lines = Vec::new();
            for _ in 0..2 {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                lines.push(serde_json::from_str::<Response>(&line).unwrap());
            }
            assert_eq!(lines[0].error.as_deref(), Some("loaded later"));
            assert!(lines[1].ok);
            drop(raw);
            assert!(a.request(&Request::Quit).unwrap().ok);
        });

        let mut fake = Fake::default();
        while !fake.quit {
            event_loop
                .dispatch(Some(Duration::from_secs(5)), &mut fake)
                .unwrap();
        }
        clients.join().unwrap();
        assert_eq!(fake.requests.len(), 9);
        assert_eq!(fake.requests[6], Request::Resume { output: None });
        assert_eq!(fake.requests[7], Request::Status);
        assert_eq!(
            fake.requests[3],
            Request::Resume {
                output: Some("DP-1".into())
            }
        );
        drop(event_loop);
        drop(guard);
        assert!(!path.exists(), "the socket file is removed");

        // A stale socket file (nobody listening) is replaced.
        drop(UnixListener::bind(&path).unwrap());
        assert!(path.exists());
        let listener = bind(&path).unwrap();
        drop(listener);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
