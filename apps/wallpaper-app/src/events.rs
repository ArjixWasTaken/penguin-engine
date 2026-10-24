//! The daemon's events, forwarded to the page: `daemon-event` with each
//! protocol event (`{event: "status", status}`, ...) and `daemon-down`
//! with a message while it isn't running. Reconnects with backoff; a
//! daemon too old to push events is polled for its status instead.

use std::time::Duration;

use tauri::{AppHandle, Emitter, Runtime};
use wpe_ipc::{Client, Event, Request};

use crate::commands::connect_error;

pub fn follow<R: Runtime>(app: AppHandle<R>) {
    std::thread::Builder::new()
        .name("daemon-events".into())
        .spawn(move || run(&app))
        .expect("spawning a thread");
}

fn run<R: Runtime>(app: &AppHandle<R>) {
    let event = |event: Event| {
        let _ = app.emit("daemon-event", event);
    };
    let down = |message: String| {
        let _ = app.emit("daemon-down", message);
    };
    let mut backoff = Duration::from_millis(500);
    loop {
        match Client::connect_default().map(Client::subscribe) {
            Ok(Ok(events)) => {
                backoff = Duration::from_millis(500);
                for e in events {
                    let Ok(e) = e else { break };
                    event(e);
                }
                down("the wallpaper daemon stopped".into());
            }
            // Connected, but it doesn't know `subscribe`.
            Ok(Err(_)) => {
                let status = Client::connect_default()
                    .and_then(|mut c| c.request(&Request::Status))
                    .map_err(|e| connect_error(&e))
                    .and_then(|r| r.into_result())
                    .and_then(|r| r.status.ok_or_else(|| "no status".to_owned()));
                match status {
                    Ok(status) => event(Event::Status { status }),
                    Err(e) => down(e),
                }
                std::thread::sleep(Duration::from_secs(3));
                continue;
            }
            Err(e) => down(connect_error(&e)),
        }
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
}
