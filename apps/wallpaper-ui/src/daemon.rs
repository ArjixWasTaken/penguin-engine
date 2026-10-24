//! Talking to the `wallpaper` daemon. Each request gets its own
//! connection on a blocking thread, so a slow `set` never holds up the rest.

use std::time::Duration;

use wpe_ipc::{Client, Event, Request, Response};

/// Loading a heavy wallpaper can take a while.
const TIMEOUT: Duration = Duration::from_secs(60);

pub async fn request(request: Request) -> Result<Response, String> {
    tokio::task::spawn_blocking(move || {
        let mut client = Client::connect_default().map_err(|e| connect_error(&e))?;
        client
            .set_timeout(Some(TIMEOUT))
            .map_err(|e| e.to_string())?;
        client
            .request(&request)
            .map_err(|e| e.to_string())?
            .into_result()
    })
    .await
    .map_err(|e| e.to_string())?
}

fn connect_error(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
            "the wallpaper daemon isn't running".to_owned()
        }
        _ => format!("connecting to the wallpaper daemon: {e}"),
    }
}

/// What the daemon pushes, or that it is gone.
#[derive(Debug, Clone)]
pub enum Feed {
    Event(Event),
    Down(String),
}

/// The daemon's events, reconnecting (with backoff) whenever it goes away.
/// A daemon too old to push events is polled for its status instead.
pub fn events() -> impl iced::futures::Stream<Item = Feed> {
    iced::stream::channel(64, async |mut output| {
        use iced::futures::SinkExt;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        std::thread::Builder::new()
            .name("daemon-events".into())
            .spawn(move || follow(&tx))
            .expect("spawning a thread");
        while let Some(feed) = rx.recv().await {
            if output.send(feed).await.is_err() {
                break;
            }
        }
    })
}

fn follow(tx: &tokio::sync::mpsc::UnboundedSender<Feed>) {
    let mut backoff = Duration::from_millis(500);
    loop {
        let subscribed = Client::connect_default().map(|client| client.subscribe());
        match subscribed {
            Ok(Ok(events)) => {
                backoff = Duration::from_millis(500);
                for event in events {
                    let Ok(event) = event else { break };
                    if tx.send(Feed::Event(event)).is_err() {
                        return;
                    }
                }
                if tx
                    .send(Feed::Down("the wallpaper daemon stopped".into()))
                    .is_err()
                {
                    return;
                }
            }
            // Connected, but it doesn't know `subscribe`.
            Ok(Err(_)) => {
                let status = Client::connect_default()
                    .and_then(|mut c| c.request(&Request::Status))
                    .map_err(|e| connect_error(&e))
                    .and_then(|r| r.into_result())
                    .and_then(|r| r.status.ok_or_else(|| "no status".to_owned()));
                let feed = match status {
                    Ok(status) => Feed::Event(Event::Status { status }),
                    Err(e) => Feed::Down(e),
                };
                if tx.send(feed).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_secs(3));
                continue;
            }
            Err(e) => {
                if tx.send(Feed::Down(connect_error(&e))).is_err() {
                    return;
                }
            }
        }
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
}
