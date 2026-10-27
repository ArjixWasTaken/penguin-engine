//! Subscribed connections ([`wpe_ipc::Request::Subscribe`]) and the events
//! sent to them. Writes never block: each subscriber has a buffer of what
//! its socket didn't take yet, flushed when the socket is writable again;
//! a subscriber that lets it grow past [`MAX_BUFFER`] (it stopped reading)
//! is dropped, so it can hold up neither the daemon nor other clients.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::rc::Rc;

use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{
    Interest, LoopHandle, Mode, PostAction, RegistrationToken,
};
use wpe_ipc::Event;

/// Bytes a subscriber may leave unread before it is dropped.
pub const MAX_BUFFER: usize = 1 << 20;

struct Subscriber {
    id: u64,
    stream: UnixStream,
    /// Written, but not taken by the socket yet.
    buffer: VecDeque<u8>,
    /// Watches for the client hanging up.
    read: Option<RegistrationToken>,
    /// Armed while `buffer` waits for the socket.
    write: Option<RegistrationToken>,
}

#[derive(Default)]
struct Inner {
    next_id: u64,
    list: Vec<Subscriber>,
}

/// The subscribed connections, served from the event loop of `D`.
pub struct Subscribers<D: 'static> {
    inner: Rc<RefCell<Inner>>,
    handle: LoopHandle<'static, D>,
}

/// What a flush left.
enum Flushed {
    Done,
    Pending,
    Failed,
}

fn flush(sub: &mut Subscriber) -> Flushed {
    while !sub.buffer.is_empty() {
        let (front, _) = sub.buffer.as_slices();
        match (&sub.stream).write(front) {
            Ok(0) => return Flushed::Failed,
            Ok(n) => {
                sub.buffer.drain(..n);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Flushed::Pending,
            Err(_) => return Flushed::Failed,
        }
    }
    if sub.buffer.is_empty() && sub.buffer.capacity() > 64 * 1024 {
        sub.buffer.shrink_to(4096);
    }
    Flushed::Done
}

impl<D: 'static> Subscribers<D> {
    pub fn new(handle: LoopHandle<'static, D>) -> Self {
        Self {
            inner: Rc::default(),
            handle,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.inner.borrow().list.is_empty()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.borrow().list.len()
    }

    /// Subscribe `stream` (its request already answered), sending it
    /// `first` before any other event.
    pub fn add(&self, stream: UnixStream, first: Option<&Event>) -> io::Result<()> {
        stream.set_nonblocking(true)?;
        let watched = stream.try_clone()?;
        let id = {
            let mut inner = self.inner.borrow_mut();
            inner.next_id += 1;
            inner.next_id
        };
        // Anything the client sends is ignored; its end is the end of the
        // subscription.
        let inner = self.inner.clone();
        let handle = self.handle.clone();
        let read = self
            .handle
            .insert_source(
                Generic::new(watched, Interest::READ, Mode::Level),
                move |_, stream, _| {
                    let mut chunk = [0u8; 4096];
                    loop {
                        match (&**stream).read(&mut chunk) {
                            Ok(0) => break,
                            Ok(_) => continue,
                            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                                return Ok(PostAction::Continue);
                            }
                            Err(_) => break,
                        }
                    }
                    drop_subscriber(&inner, &handle, id);
                    Ok(PostAction::Continue)
                },
            )
            .map_err(|e| io::Error::other(format!("cannot watch a subscriber: {}", e.error)))?;
        self.inner.borrow_mut().list.push(Subscriber {
            id,
            stream,
            buffer: VecDeque::new(),
            read: Some(read),
            write: None,
        });
        tracing::debug!("subscriber {id} added");
        if let Some(event) = first {
            self.send_to(event, Some(id));
        }
        Ok(())
    }

    /// Disconnect every subscriber.
    pub fn close_all(&self) {
        let ids: Vec<u64> = self.inner.borrow().list.iter().map(|s| s.id).collect();
        for id in ids {
            drop_subscriber(&self.inner, &self.handle, id);
        }
    }

    /// Send `event` to every subscriber.
    pub fn send(&self, event: &Event) {
        self.send_to(event, None);
    }

    fn send_to(&self, event: &Event, only: Option<u64>) {
        if self.is_empty() {
            return;
        }
        let line = wpe_ipc::encode_line(event);
        let mut gone = Vec::new();
        let mut waiting = Vec::new();
        {
            let mut inner = self.inner.borrow_mut();
            for sub in &mut inner.list {
                if only.is_some_and(|id| id != sub.id) {
                    continue;
                }
                if sub.buffer.len() + line.len() > MAX_BUFFER {
                    tracing::warn!("subscriber {} doesn't read its events; dropping it", sub.id);
                    gone.push(sub.id);
                    continue;
                }
                sub.buffer.extend(line.as_bytes());
                match flush(sub) {
                    Flushed::Done => {}
                    Flushed::Pending => {
                        if sub.write.is_none() {
                            waiting.push(sub.id);
                        }
                    }
                    Flushed::Failed => gone.push(sub.id),
                }
            }
        }
        for id in gone {
            drop_subscriber(&self.inner, &self.handle, id);
        }
        for id in waiting {
            self.watch_writable(id);
        }
    }

    /// Flush `id`'s buffer whenever its socket takes more.
    fn watch_writable(&self, id: u64) {
        let stream = {
            let inner = self.inner.borrow();
            let Some(sub) = inner.list.iter().find(|s| s.id == id) else {
                return;
            };
            sub.stream.try_clone()
        };
        let Ok(stream) = stream else {
            drop_subscriber(&self.inner, &self.handle, id);
            return;
        };
        let inner = self.inner.clone();
        let handle = self.handle.clone();
        let inserted = self.handle.insert_source(
            Generic::new(stream, Interest::WRITE, Mode::Level),
            move |_, _, _| {
                let result = {
                    let mut list = inner.borrow_mut();
                    match list.list.iter_mut().find(|s| s.id == id) {
                        Some(sub) => flush(sub),
                        // Gone; its sources are being removed.
                        None => return Ok(PostAction::Continue),
                    }
                };
                match result {
                    Flushed::Pending => {}
                    Flushed::Done => {
                        let mut list = inner.borrow_mut();
                        if let Some(sub) = list.list.iter_mut().find(|s| s.id == id)
                            && let Some(token) = sub.write.take()
                        {
                            remove_later(&handle, token);
                        }
                    }
                    Flushed::Failed => drop_subscriber(&inner, &handle, id),
                }
                Ok(PostAction::Continue)
            },
        );
        match inserted {
            Ok(token) => {
                let mut inner = self.inner.borrow_mut();
                match inner.list.iter_mut().find(|s| s.id == id) {
                    Some(sub) => sub.write = Some(token),
                    None => remove_later(&self.handle, token),
                }
            }
            Err(e) => {
                tracing::warn!("cannot watch a subscriber: {}", e.error);
                drop_subscriber(&self.inner, &self.handle, id);
            }
        }
    }
}

/// Remove a source after this loop iteration (never from inside its own
/// callback, and exactly once).
fn remove_later<D: 'static>(handle: &LoopHandle<'static, D>, token: RegistrationToken) {
    let h = handle.clone();
    handle.insert_idle(move |_| h.remove(token));
}

fn drop_subscriber<D: 'static>(
    inner: &Rc<RefCell<Inner>>,
    handle: &LoopHandle<'static, D>,
    id: u64,
) {
    let removed = {
        let mut inner = inner.borrow_mut();
        let Some(i) = inner.list.iter().position(|s| s.id == id) else {
            return;
        };
        inner.list.remove(i)
    };
    // The sources hold copies of the socket: the client sees the end now.
    let _ = removed.stream.shutdown(std::net::Shutdown::Both);
    for token in [removed.read, removed.write].into_iter().flatten() {
        remove_later(handle, token);
    }
    tracing::debug!("subscriber {id} gone");
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay_client_toolkit::reexports::calloop::EventLoop;
    use std::io::{BufRead, BufReader};
    use std::time::Duration;
    use wpe_ipc::Status;

    fn status(n: usize) -> Event {
        let mut status = Status::default();
        status.settings.fps = n as u32;
        Event::Status { status }
    }

    fn spin(event_loop: &mut EventLoop<'static, ()>) {
        for _ in 0..5 {
            event_loop
                .dispatch(Some(Duration::from_millis(5)), &mut ())
                .unwrap();
        }
    }

    #[test]
    fn a_stuck_subscriber_is_dropped_others_get_everything() {
        let mut event_loop: EventLoop<'static, ()> = EventLoop::try_new().unwrap();
        let subs = Subscribers::new(event_loop.handle());
        let (stuck, stuck_peer) = UnixStream::pair().unwrap();
        let (good, good_peer) = UnixStream::pair().unwrap();
        subs.add(stuck, Some(&status(0))).unwrap();
        subs.add(good, None).unwrap();
        assert_eq!(subs.len(), 2);

        // The good one reads on a thread; the stuck one never does.
        let reader = std::thread::spawn(move || {
            let mut reader = BufReader::new(good_peer);
            let mut count = 0;
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap() > 0 {
                let event: Event = serde_json::from_str(&line).unwrap();
                assert_eq!(event, status(count));
                count += 1;
                line.clear();
            }
            count
        });
        // Each event is ~100 bytes: well past the buffer and the socket's.
        // Sending never blocks; the good one gets its events as it reads.
        let events = 4 * MAX_BUFFER / 100;
        let flushing = |subs: &Subscribers<()>| {
            subs.inner
                .borrow()
                .list
                .iter()
                .any(|s| s.id == 2 && s.write.is_some())
        };
        for n in 0..events {
            subs.send(&status(n));
            if n % 1000 == 999 {
                while flushing(&subs) {
                    event_loop
                        .dispatch(Some(Duration::from_millis(50)), &mut ())
                        .unwrap();
                }
            }
        }
        assert_eq!(subs.len(), 1, "the stuck subscriber is dropped");
        assert_eq!(subs.inner.borrow().list[0].id, 2);
        while flushing(&subs) {
            event_loop
                .dispatch(Some(Duration::from_millis(50)), &mut ())
                .unwrap();
        }
        subs.close_all();
        spin(&mut event_loop);
        assert_eq!(reader.join().unwrap(), events);
        drop(stuck_peer);
    }

    #[test]
    fn hanging_up_unsubscribes() {
        let mut event_loop: EventLoop<'static, ()> = EventLoop::try_new().unwrap();
        let subs = Subscribers::new(event_loop.handle());
        let (a, a_peer) = UnixStream::pair().unwrap();
        subs.add(a, Some(&status(7))).unwrap();
        let mut reader = BufReader::new(a_peer.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(serde_json::from_str::<Event>(&line).unwrap(), status(7));
        // What a subscriber sends is ignored.
        (&a_peer).write_all(b"{\"cmd\":\"quit\"}\n").unwrap();
        spin(&mut event_loop);
        assert_eq!(subs.len(), 1);
        drop(reader);
        drop(a_peer);
        spin(&mut event_loop);
        assert!(subs.is_empty());
        subs.send(&status(1));
    }

    /// A daemon stand-in: `pause` changes the status, which subscribers get
    /// after the loop iteration.
    struct Fake {
        events: Subscribers<Fake>,
        pauses: usize,
        dirty: bool,
        quit: bool,
    }

    impl crate::control::Handler for Fake {
        fn handle_request(
            &mut self,
            request: wpe_ipc::Request,
            responder: crate::control::Responder,
        ) {
            use wpe_ipc::{Request, Response};
            match request {
                Request::Subscribe => {
                    if let Some(stream) = responder.into_subscriber() {
                        self.events.add(stream, None).unwrap();
                    }
                }
                Request::Pause { .. } => {
                    self.pauses += 1;
                    self.dirty = true;
                    responder.send(Response::ok());
                }
                Request::Quit => {
                    self.quit = true;
                    responder.send(Response::ok());
                }
                _ => responder.send(Response::ok()),
            }
        }
    }

    impl Fake {
        /// A status as big as one with many outputs.
        fn status(&self) -> Event {
            let mut status = Status::default();
            status.settings.fps = self.pauses as u32;
            status.outputs = (0..50)
                .map(|n| wpe_ipc::OutputStatus {
                    name: format!("DP-{n}"),
                    paused: true,
                    ..Default::default()
                })
                .collect();
            Event::Status { status }
        }
    }

    #[test]
    fn subscribers_hear_of_other_clients_changes_and_a_stuck_one_blocks_nobody() {
        use crate::control;
        use wpe_ipc::{Client, Request};

        let dir = std::env::temp_dir().join(format!("wpe-events-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("test.sock");
        let mut event_loop: EventLoop<'static, Fake> = EventLoop::try_new().unwrap();
        let socket = control::listen_at(&path).unwrap();
        let guard = control::serve(socket, &event_loop.handle()).unwrap();
        let mut fake = Fake {
            events: Subscribers::new(event_loop.handle()),
            pauses: 0,
            dirty: false,
            quit: false,
        };

        let client_path = path.clone();
        let clients = std::thread::spawn(move || {
            // One that subscribes and never reads again.
            let mut stuck = UnixStream::connect(&client_path).unwrap();
            stuck.write_all(b"{\"cmd\":\"subscribe\"}\n").unwrap();
            let mut events = Client::connect(&client_path).unwrap().subscribe().unwrap();
            let mut other = Client::connect(&client_path).unwrap();

            assert!(other.request(&Request::Pause { output: None }).unwrap().ok);
            match events.next().unwrap().unwrap() {
                Event::Status { status } => {
                    assert_eq!(status.settings.fps, 1);
                    assert!(status.outputs[0].paused);
                }
                e => panic!("{e:?}"),
            }
            // ~4 KB each: far more than the stuck one's buffer holds. The
            // other client is answered all along, and the reader gets
            // every status, the last one last.
            let reader = std::thread::spawn(move || {
                let mut last = 0;
                for event in events {
                    if let Event::Status { status } = event.unwrap() {
                        last = status.settings.fps;
                    }
                }
                last
            });
            let pauses = 2 + 3 * MAX_BUFFER / 4000;
            for _ in 1..pauses {
                assert!(other.request(&Request::Pause { output: None }).unwrap().ok);
            }
            assert!(other.request(&Request::Quit).unwrap().ok);
            (reader, pauses, stuck)
        });

        while !fake.quit {
            event_loop
                .dispatch(Some(Duration::from_secs(5)), &mut fake)
                .unwrap();
            if std::mem::take(&mut fake.dirty) {
                let status = fake.status();
                fake.events.send(&status);
            }
        }
        let (reader, pauses, stuck) = clients.join().unwrap();
        assert_eq!(fake.pauses, pauses);
        // Flush the reader's last events, then hang up on it.
        assert_eq!(fake.events.len(), 1, "the stuck subscriber was dropped");
        drop(stuck);
        while fake
            .events
            .inner
            .borrow()
            .list
            .iter()
            .any(|s| s.write.is_some())
        {
            event_loop
                .dispatch(Some(Duration::from_millis(50)), &mut fake)
                .unwrap();
        }
        fake.events.close_all();
        assert_eq!(reader.join().unwrap(), pauses as u32);
        drop(guard);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
