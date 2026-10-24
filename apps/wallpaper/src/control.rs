//! The control socket: line-delimited JSON requests ([`wpe_ipc::Request`])
//! from any number of clients, served from the event loop.

use std::io::{self, Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{Interest, LoopHandle, Mode, PostAction};
use wpe_ipc::{Request, Response};

/// Requests longer than this are refused (and the client dropped).
const MAX_LINE: usize = 1 << 20;
/// A client that doesn't read its responses for this long is dropped.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// Whoever answers requests: the daemon.
pub trait Handler {
    fn handle_request(&mut self, request: Request) -> Response;
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
            Ok(_) => bail!(
                "another wallpaper daemon is listening on {}",
                path.display()
            ),
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
    let mut pending = Vec::new();
    handle
        .insert_source(
            Generic::new(stream, Interest::READ, Mode::Level),
            move |_, stream, handler| {
                let mut chunk = [0u8; 8192];
                let n = match (&**stream).read(&mut chunk) {
                    Ok(0) => return Ok(PostAction::Remove),
                    Ok(n) => n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                        return Ok(PostAction::Continue);
                    }
                    Err(_) => return Ok(PostAction::Remove),
                };
                pending.extend_from_slice(&chunk[..n]);
                let mut out = String::new();
                while let Some(end) = pending.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = pending.drain(..=end).collect();
                    if let Some(response) = respond(handler, &line[..end]) {
                        out.push_str(&wpe_ipc::encode_line(&response));
                    }
                }
                if pending.len() > MAX_LINE {
                    out.push_str(&wpe_ipc::encode_line(&Response::error("request too long")));
                    let _ = (&**stream).write_all(out.as_bytes());
                    return Ok(PostAction::Remove);
                }
                if !out.is_empty() && (&**stream).write_all(out.as_bytes()).is_err() {
                    return Ok(PostAction::Remove);
                }
                Ok(PostAction::Continue)
            },
        )
        .map_err(|e| anyhow::anyhow!("cannot watch a control client: {}", e.error))?;
    Ok(())
}

/// The response to one request line; none for a blank line.
fn respond<D: Handler>(handler: &mut D, line: &[u8]) -> Option<Response> {
    let line = std::str::from_utf8(line).ok().map(str::trim);
    match line {
        Some("") => None,
        Some(line) => Some(match serde_json::from_str::<Request>(line) {
            Ok(request) => {
                tracing::debug!("control request: {request:?}");
                handler.handle_request(request)
            }
            Err(e) => Response::error(format!("bad request: {e}")),
        }),
        None => Some(Response::error("bad request: not UTF-8")),
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
    }

    impl Handler for Fake {
        fn handle_request(&mut self, request: Request) -> Response {
            self.requests.push(request.clone());
            match request {
                Request::Quit => {
                    self.quit = true;
                    Response::ok()
                }
                Request::Fps { fps } if fps > 1000 => Response::error("too fast"),
                _ => Response::ok(),
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
            assert!(a.request(&Request::Quit).unwrap().ok);
        });

        let mut fake = Fake::default();
        while !fake.quit {
            event_loop
                .dispatch(Some(Duration::from_secs(5)), &mut fake)
                .unwrap();
        }
        clients.join().unwrap();
        assert_eq!(fake.requests.len(), 5);
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
