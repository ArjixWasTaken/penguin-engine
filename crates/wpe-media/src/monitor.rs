//! The background MPRIS watcher.
//!
//! Threads:
//! - two signal readers (NameOwnerChanged for `org.mpris.MediaPlayer2.*`, and
//!   every signal on `/org/mpris/MediaPlayer2`, which covers
//!   `PropertiesChanged` and `Seeked`), each blocking on a zbus
//!   `MessageIterator` and forwarding into the worker's channel;
//! - the worker, which owns the [`Tracker`], makes the D-Bus method calls and
//!   publishes events; it sleeps in `recv` and only wakes on a timeout to
//!   poll `Position` (once a second, and only while the followed player is
//!   playing);
//! - the art loader, which fetches and decodes `mpris:artUrl` off the
//!   worker so a slow download never delays other events.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use zbus::blocking::{Connection, MessageIterator};
use zbus::message::Type as MsgType;
use zbus::zvariant::OwnedValue;
use zbus::{MatchRule, Message};

use crate::MediaError;
use crate::art::{self, Thumbnail};
use crate::event::{MediaEvent, MediaEventKind, ThumbnailColors, coalesce};
use crate::mpris::{self, BUS_PREFIX, OBJECT_PATH, PLAYER_IFACE};
use crate::tracker::{ArtChange, Tracker};

/// How often `Position` is polled while playing (MPRIS doesn't signal it).
pub const POSITION_POLL: Duration = Duration::from_secs(1);
const METHOD_TIMEOUT: Duration = Duration::from_secs(2);
const START_TIMEOUT: Duration = Duration::from_secs(5);
const PROPS_IFACE: &str = "org.freedesktop.DBus.Properties";

enum Msg {
    Signal(Message),
    Art {
        url: String,
        result: Result<Thumbnail, MediaError>,
    },
    Shutdown,
}

#[derive(Default)]
struct Shared {
    events: Mutex<Vec<MediaEvent>>,
    thumbnail: Mutex<Option<Arc<Thumbnail>>>,
    /// Generation of `thumbnail` (0 when there is none).
    generation: AtomicU64,
}

/// Watches MPRIS players on the session bus and turns their state into
/// SceneScript media events. Dropping it stops the background threads.
pub struct MediaMonitor {
    shared: Arc<Shared>,
    tx: Sender<Msg>,
    worker: Option<JoinHandle<()>>,
}

impl MediaMonitor {
    /// Connects to the session bus and starts watching. Fails (without
    /// spawning anything long-lived) when there is no session bus.
    ///
    /// When this returns, the initial state (status, playback, properties,
    /// timeline, and the thumbnail when there's no art to load) is already
    /// queued for the first [`MediaMonitor::poll`]; a thumbnail event for
    /// existing art follows once it's decoded.
    pub fn start() -> Result<Self, MediaError> {
        let conn = zbus::blocking::connection::Builder::session()?
            .method_timeout(METHOD_TIMEOUT)
            .build()?;

        // Subscribe before enumerating so no change is missed in between.
        let owner_rule = MatchRule::builder()
            .msg_type(MsgType::Signal)
            .sender("org.freedesktop.DBus")?
            .interface("org.freedesktop.DBus")?
            .member("NameOwnerChanged")?
            .arg0ns("org.mpris.MediaPlayer2")?
            .build();
        let player_rule = MatchRule::builder()
            .msg_type(MsgType::Signal)
            .path(OBJECT_PATH)?
            .build();
        let owner_iter = MessageIterator::for_match_rule(owner_rule, &conn, Some(256))?;
        let player_iter = MessageIterator::for_match_rule(player_rule, &conn, Some(256))?;

        let (tx, rx) = mpsc::channel();
        for (name, iter) in [
            ("wpe-media-names", owner_iter),
            ("wpe-media-props", player_iter),
        ] {
            let tx = tx.clone();
            spawn(name, move || {
                for msg in iter {
                    match msg {
                        Ok(m) => {
                            if tx.send(Msg::Signal(m)).is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            tracing::debug!("media: signal stream ended: {e}");
                            break;
                        }
                    }
                }
            })?;
        }

        let (art_tx, art_rx) = mpsc::channel::<String>();
        {
            let tx = tx.clone();
            spawn("wpe-media-art", move || art_loader(art_rx, tx))?;
        }

        let shared = Arc::new(Shared::default());
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker = {
            let shared = shared.clone();
            spawn("wpe-media", move || {
                let mut w = Worker {
                    conn,
                    tracker: Tracker::new(),
                    shared,
                    art_tx,
                    wanted_art: None,
                    generation: 0,
                };
                w.enumerate();
                w.sync();
                let _ = ready_tx.send(());
                w.run(rx);
                if let Err(e) = w.conn.close() {
                    tracing::debug!("media: closing bus connection: {e}");
                }
            })?
        };
        if ready_rx.recv_timeout(START_TIMEOUT).is_err() {
            tracing::warn!("media: initial player scan is slow; continuing in the background");
        }
        Ok(Self {
            shared,
            tx,
            worker: Some(worker),
        })
    }

    /// Drains the events produced since the last call, oldest first.
    /// Superseded events are coalesced: at most one event of each kind is
    /// returned (the latest), so at most one timeline event per poll.
    pub fn poll(&mut self) -> Vec<MediaEvent> {
        let events = std::mem::take(&mut *lock(&self.shared.events));
        coalesce(events)
    }

    /// Current album art, if any.
    pub fn thumbnail(&self) -> Option<Arc<Thumbnail>> {
        lock(&self.shared.thumbnail).clone()
    }

    /// Generation of the current album art (0 when there is none). Cheap;
    /// compare against the last uploaded value to know when to re-upload.
    pub fn thumbnail_generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Acquire)
    }
}

impl Drop for MediaMonitor {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Shutdown);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> Result<JoinHandle<()>, MediaError> {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(f)
        .map_err(|e| MediaError::Thread(e.to_string()))
}

/// Loads the most recently requested art URL; older queued requests are
/// skipped.
fn art_loader(rx: Receiver<String>, tx: Sender<Msg>) {
    while let Ok(mut url) = rx.recv() {
        while let Ok(newer) = rx.try_recv() {
            url = newer;
        }
        let mut result = art::load(&url);
        // Players that write art to a temp file sometimes signal first.
        if matches!(art::parse_art_url(&url), Ok(art::ArtSource::File(_))) {
            for _ in 0..3 {
                if result.is_ok() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(300));
                // If a newer art request arrived while sleeping, drop stale retry.
                if let Ok(newer) = rx.try_recv() {
                    url = newer;
                    while let Ok(latest) = rx.try_recv() {
                        url = latest;
                    }
                }
                result = art::load(&url);
            }
        }
        if tx.send(Msg::Art { url, result }).is_err() {
            break;
        }
    }
}

struct Worker {
    conn: Connection,
    tracker: Tracker,
    shared: Arc<Shared>,
    art_tx: Sender<String>,
    wanted_art: Option<String>,
    generation: u64,
}

impl Worker {
    fn run(&mut self, rx: Receiver<Msg>) {
        let mut next_poll = Instant::now() + POSITION_POLL;
        loop {
            let msg = if self.tracker.selected_is_playing() {
                let now = Instant::now();
                if next_poll <= now {
                    self.poll_position();
                    self.sync();
                    next_poll = now + POSITION_POLL;
                    continue;
                }
                match rx.recv_timeout(next_poll - now) {
                    Ok(m) => m,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            } else {
                match rx.recv() {
                    Ok(m) => m,
                    Err(_) => return,
                }
            };
            match msg {
                Msg::Shutdown => return,
                Msg::Signal(m) => {
                    let before = self.tracker.selected().map(str::to_string);
                    let touched = self.handle_signal(&m);
                    self.sync();
                    let after = self.tracker.selected().map(str::to_string);
                    // A new player, track or play state: refresh the position
                    // now instead of waiting for the next poll.
                    if after.is_some() && (before != after || touched == after) {
                        self.poll_position();
                        self.sync();
                        next_poll = Instant::now() + POSITION_POLL;
                    }
                }
                Msg::Art { url, result } => self.art_loaded(url, result),
            }
        }
    }

    /// `org.freedesktop.DBus.Properties` call on a player's MPRIS object.
    fn props_call<B, R>(&self, dest: &str, method: &str, body: &B) -> Option<R>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
        R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
    {
        match self
            .conn
            .call_method(Some(dest), OBJECT_PATH, Some(PROPS_IFACE), method, body)
        {
            Ok(r) => r
                .body()
                .deserialize::<R>()
                .map_err(|e| tracing::debug!("media: {dest} {method}: bad reply: {e}"))
                .ok(),
            Err(e) => {
                tracing::debug!("media: {dest} {method}: {e}");
                None
            }
        }
    }

    fn enumerate(&mut self) {
        let names: Vec<String> = match self.conn.call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "ListNames",
            &(),
        ) {
            Ok(r) => r.body().deserialize().unwrap_or_default(),
            Err(e) => {
                tracing::warn!("media: ListNames failed: {e}");
                return;
            }
        };
        for name in names.into_iter().filter(|n| n.starts_with(BUS_PREFIX)) {
            let owner: Option<String> = self
                .conn
                .call_method(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    Some("org.freedesktop.DBus"),
                    "GetNameOwner",
                    &(name.as_str(),),
                )
                .ok()
                .and_then(|r| r.body().deserialize().ok());
            if let Some(owner) = owner {
                self.tracker.upsert(&name, &owner, false);
                self.refresh(&name);
            }
        }
    }

    /// Re-reads all Player properties of one player.
    fn refresh(&mut self, name: &str) {
        let props: Option<HashMap<String, OwnedValue>> =
            self.props_call(name, "GetAll", &(PLAYER_IFACE,));
        if let Some(props) = props {
            self.tracker.update(name, mpris::parse_player_props(&props));
        }
    }

    fn poll_position(&mut self) {
        let Some(name) = self.tracker.selected().map(str::to_string) else {
            return;
        };
        let v: Option<OwnedValue> = self.props_call(&name, "Get", &(PLAYER_IFACE, "Position"));
        if let Some(pos) = v.as_deref().and_then(mpris::parse_position) {
            self.tracker.update(
                &name,
                crate::tracker::PlayerUpdate {
                    position_us: Some(pos),
                    ..Default::default()
                },
            );
        }
    }

    /// Applies one signal. Returns the well-known name of the player whose
    /// playback/track changed, if any.
    fn handle_signal(&mut self, m: &Message) -> Option<String> {
        let header = m.header();
        let member = header.member()?.as_str().to_string();
        let iface = header.interface().map(|i| i.as_str().to_string());
        let sender = header.sender().map(|s| s.as_str().to_string());
        drop(header);

        match (iface.as_deref(), member.as_str()) {
            (Some("org.freedesktop.DBus"), "NameOwnerChanged") => {
                let (name, _old, new): (String, String, String) = m.body().deserialize().ok()?;
                if !name.starts_with(BUS_PREFIX) {
                    return None;
                }
                if new.is_empty() {
                    tracing::debug!("media: player gone: {name}");
                    self.tracker.remove(&name);
                    None
                } else {
                    tracing::debug!("media: player appeared: {name}");
                    self.tracker.upsert(&name, &new, true);
                    self.refresh(&name);
                    Some(name)
                }
            }
            (Some(PROPS_IFACE), "PropertiesChanged") => {
                let (iface, changed, invalidated): (
                    String,
                    HashMap<String, OwnedValue>,
                    Vec<String>,
                ) = m.body().deserialize().ok()?;
                if iface != PLAYER_IFACE {
                    return None;
                }
                let names = self.tracker.names_for_owner(sender.as_deref()?);
                let update = mpris::parse_player_props(&changed);
                let relevant = update.playback.is_some() || update.track.is_some();
                let mut touched = None;
                for name in names {
                    self.tracker.update(&name, update.clone());
                    if !invalidated.is_empty() {
                        self.refresh(&name);
                    }
                    if relevant || !invalidated.is_empty() {
                        touched = Some(name);
                    }
                }
                touched
            }
            (Some(PLAYER_IFACE), "Seeked") => {
                let (pos,): (i64,) = m.body().deserialize().ok()?;
                for name in self.tracker.names_for_owner(sender.as_deref()?) {
                    self.tracker.update(
                        &name,
                        crate::tracker::PlayerUpdate {
                            position_us: Some(pos),
                            ..Default::default()
                        },
                    );
                }
                None
            }
            _ => None,
        }
    }

    fn push(&self, events: Vec<MediaEvent>) {
        if !events.is_empty() {
            lock(&self.shared.events).extend(events);
        }
    }

    fn sync(&mut self) {
        let out = self.tracker.sync();
        // Keep the documented order: ..., thumbnail, timeline.
        let (timeline, rest): (Vec<_>, Vec<_>) = out
            .events
            .into_iter()
            .partition(|e| e.kind() == MediaEventKind::Timeline);
        self.push(rest);
        match out.art {
            None => {}
            Some(ArtChange::Load(url)) => {
                self.wanted_art = Some(url.clone());
                let _ = self.art_tx.send(url);
            }
            Some(ArtChange::Clear) => {
                self.wanted_art = None;
                self.set_thumbnail(None);
            }
        }
        self.push(timeline);
    }

    fn art_loaded(&mut self, url: String, result: Result<Thumbnail, MediaError>) {
        if self.wanted_art.as_deref() != Some(url.as_str()) {
            return; // stale
        }
        match result {
            Ok(t) => self.set_thumbnail(Some(t)),
            Err(e) => {
                tracing::warn!("media: {e}");
                self.set_thumbnail(None);
            }
        }
    }

    fn set_thumbnail(&mut self, t: Option<Thumbnail>) {
        let event = match t {
            Some(mut t) => {
                self.generation += 1;
                t.generation = self.generation;
                let ev = MediaEvent::Thumbnail {
                    has_thumbnail: true,
                    colors: t.colors,
                    generation: t.generation,
                };
                *lock(&self.shared.thumbnail) = Some(Arc::new(t));
                self.shared
                    .generation
                    .store(self.generation, Ordering::Release);
                ev
            }
            None => {
                *lock(&self.shared.thumbnail) = None;
                self.shared.generation.store(0, Ordering::Release);
                MediaEvent::Thumbnail {
                    has_thumbnail: false,
                    colors: ThumbnailColors::NEUTRAL,
                    generation: 0,
                }
            }
        };
        self.push(vec![event]);
    }
}
