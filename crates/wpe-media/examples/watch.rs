//! Prints media events as JSON lines.
//!
//! `cargo run -p wpe-media --example watch [seconds]` (default 20).

use std::time::{Duration, Instant};

fn main() {
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let mut monitor = match wpe_media::MediaMonitor::start() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("media monitor unavailable: {e}");
            std::process::exit(1);
        }
    };
    let start = Instant::now();
    let mut last_gen = 0;
    while start.elapsed() < Duration::from_secs(secs) {
        for ev in monitor.poll() {
            let line = serde_json::json!({
                "t": (start.elapsed().as_secs_f64() * 1000.0).round() / 1000.0,
                "handler": ev.handler_name(),
                "event": ev.to_json(),
            });
            println!("{line}");
        }
        let generation = monitor.thumbnail_generation();
        if generation != last_gen {
            last_gen = generation;
            match monitor.thumbnail() {
                Some(t) => println!(
                    "{}",
                    serde_json::json!({
                        "thumbnail": { "width": t.width, "height": t.height,
                                       "bytes": t.rgba.len(), "generation": t.generation }
                    })
                ),
                None => println!("{}", serde_json::json!({ "thumbnail": null })),
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
