//! End-to-end test against a fake MPRIS player served on the real session
//! bus. Skipped when there's no session bus, or when another player is
//! already active (the monitor would rightly follow that one).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wpe_media::{MediaEvent, MediaMonitor, PlaybackState};
use zbus::zvariant::{OwnedValue, Value};

const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

struct State {
    status: String,
    metadata: HashMap<String, OwnedValue>,
    position: i64,
}

struct Player(Arc<Mutex<State>>);

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    #[zbus(property)]
    fn playback_status(&self) -> String {
        self.0.lock().unwrap().status.clone()
    }
    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        let s = self.0.lock().unwrap();
        s.metadata
            .iter()
            .map(|(k, v)| (k.clone(), v.try_clone().unwrap()))
            .collect()
    }
    #[zbus(property)]
    fn position(&self) -> i64 {
        self.0.lock().unwrap().position
    }
}

fn owned(v: Value<'_>) -> OwnedValue {
    v.try_to_owned().unwrap()
}

fn red_png_data_url() -> String {
    let img = image::RgbaImage::from_pixel(8, 8, image::Rgba([255, 0, 0, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    let bytes = out.into_inner();
    let hex: String = bytes.iter().map(|b| format!("%{b:02X}")).collect();
    // Percent-encoded (non-base64) data URL.
    format!("data:image/png,{hex}")
}

/// Polls until `pred` matches an event, collecting everything seen.
fn wait_for(
    m: &mut MediaMonitor,
    seen: &mut Vec<MediaEvent>,
    pred: impl Fn(&MediaEvent) -> bool,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let evs = m.poll();
        let hit = evs.iter().any(&pred);
        seen.extend(evs);
        if hit {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn follows_a_fake_player() {
    let mut monitor = match MediaMonitor::start() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("skipping: {e}");
            return;
        }
    };
    let initial = monitor.poll();
    if initial.contains(&MediaEvent::Status { enabled: true }) {
        eprintln!("skipping: a real media player is active");
        return;
    }
    assert!(
        initial.contains(&MediaEvent::Status { enabled: false }),
        "{initial:?}"
    );
    assert!(
        initial.iter().any(|e| matches!(
            e,
            MediaEvent::Thumbnail {
                has_thumbnail: false,
                ..
            }
        )),
        "{initial:?}"
    );

    let mut md = HashMap::new();
    md.insert("xesam:title".to_string(), owned(Value::from("Test Song")));
    md.insert(
        "xesam:artist".to_string(),
        owned(Value::from(vec!["Tester"])),
    );
    md.insert(
        "mpris:length".to_string(),
        owned(Value::from(100_000_000i64)),
    );
    md.insert(
        "mpris:artUrl".to_string(),
        owned(Value::from(red_png_data_url())),
    );
    let state = Arc::new(Mutex::new(State {
        status: "Playing".into(),
        metadata: md,
        position: 3_000_000,
    }));
    let name = format!(
        "org.mpris.MediaPlayer2.wpe_media_test_{}",
        std::process::id()
    );
    let conn = zbus::blocking::connection::Builder::session()
        .unwrap()
        .serve_at(PATH, Player(state.clone()))
        .unwrap()
        .name(name.as_str())
        .unwrap()
        .build()
        .unwrap();

    let mut seen = Vec::new();
    assert!(
        wait_for(&mut monitor, &mut seen, |e| matches!(
            e,
            MediaEvent::Thumbnail {
                has_thumbnail: true,
                ..
            }
        )),
        "{seen:?}"
    );
    assert!(
        seen.contains(&MediaEvent::Status { enabled: true }),
        "{seen:?}"
    );
    assert!(
        seen.contains(&MediaEvent::Playback {
            state: PlaybackState::Playing
        }),
        "{seen:?}"
    );
    assert!(
        seen.iter().any(|e| matches!(e, MediaEvent::Properties(p)
            if p.title == "Test Song" && p.artist == "Tester")),
        "{seen:?}"
    );
    assert!(
        seen.iter()
            .any(|e| matches!(e, MediaEvent::Timeline { position, duration }
            if *position == 3.0 && *duration == 100.0)),
        "{seen:?}"
    );
    let art = monitor.thumbnail().expect("thumbnail");
    assert_eq!((art.width, art.height), (8, 8));
    assert_eq!(art.colors.primary, [1.0, 0.0, 0.0]);
    assert_eq!(monitor.thumbnail_generation(), art.generation);

    // Position is polled while playing.
    state.lock().unwrap().position = 7_000_000;
    seen.clear();
    assert!(
        wait_for(&mut monitor, &mut seen, |e| matches!(
            e,
            MediaEvent::Timeline { position, .. } if *position == 7.0
        )),
        "{seen:?}"
    );

    // Pause via PropertiesChanged.
    state.lock().unwrap().status = "Paused".into();
    let mut changed: HashMap<&str, Value<'_>> = HashMap::new();
    changed.insert("PlaybackStatus", Value::from("Paused"));
    conn.emit_signal(
        None::<&str>,
        PATH,
        "org.freedesktop.DBus.Properties",
        "PropertiesChanged",
        &(PLAYER, changed, Vec::<String>::new()),
    )
    .unwrap();
    seen.clear();
    assert!(
        wait_for(&mut monitor, &mut seen, |e| *e
            == MediaEvent::Playback {
                state: PlaybackState::Paused
            }),
        "{seen:?}"
    );

    // Seeked.
    conn.emit_signal(None::<&str>, PATH, PLAYER, "Seeked", &(50_000_000i64,))
        .unwrap();
    seen.clear();
    assert!(
        wait_for(&mut monitor, &mut seen, |e| matches!(
            e,
            MediaEvent::Timeline { position, .. } if *position == 50.0
        )),
        "{seen:?}"
    );

    // Player quits.
    drop(conn);
    seen.clear();
    assert!(
        wait_for(&mut monitor, &mut seen, |e| *e
            == MediaEvent::Status { enabled: false }),
        "{seen:?}"
    );
    assert!(monitor.thumbnail().is_none());
    assert_eq!(monitor.thumbnail_generation(), 0);

    let t = Instant::now();
    drop(monitor);
    assert!(
        t.elapsed() < Duration::from_secs(1),
        "drop took {:?}",
        t.elapsed()
    );
}
