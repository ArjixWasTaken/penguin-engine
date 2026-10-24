//! Wallpaper pictures for the page: `thumb://localhost/<id>` is a small
//! PNG still (cached; see [`wpe_library::thumbs`]) and
//! `preview://localhost/<id>` the preview file as it is (GIFs animate).

use std::sync::{Arc, LazyLock};

use tauri::http::{Request, Response, StatusCode, header};
use tauri::{Manager, Runtime, UriSchemeContext, UriSchemeResponder};
use tokio::sync::Semaphore;

use crate::commands::Library;

/// A picture's bytes and media type, made from a preview file.
type Make = fn(&std::path::Path) -> Option<(Vec<u8>, &'static str)>;

/// Thumbnails made at once; the rest wait their turn.
static WORKERS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(4)));

pub fn thumb<R: Runtime>(
    ctx: UriSchemeContext<'_, R>,
    request: Request<Vec<u8>>,
    responder: UriSchemeResponder,
) {
    serve(ctx, request, responder, |preview| {
        let file = wpe_library::thumbs::ensure(preview)?;
        Some((std::fs::read(file).ok()?, "image/png"))
    });
}

pub fn preview<R: Runtime>(
    ctx: UriSchemeContext<'_, R>,
    request: Request<Vec<u8>>,
    responder: UriSchemeResponder,
) {
    serve(ctx, request, responder, |preview| {
        Some((std::fs::read(preview).ok()?, mime(preview)))
    });
}

/// Answer with what `make` gives for the preview of the item the path names.
fn serve<R: Runtime>(
    ctx: UriSchemeContext<'_, R>,
    request: Request<Vec<u8>>,
    responder: UriSchemeResponder,
    make: Make,
) {
    let app = ctx.app_handle().clone();
    let id = request.uri().path().trim_start_matches('/').to_owned();
    tauri::async_runtime::spawn(async move {
        let loaded = app.state::<Library>().loaded(false).await;
        let preview = loaded.get(&id).and_then(|item| item.preview.clone());
        let body = match preview {
            Some(preview) => {
                let _permit = WORKERS.clone().acquire_owned().await.ok();
                tauri::async_runtime::spawn_blocking(move || make(&preview))
                    .await
                    .ok()
                    .flatten()
            }
            None => None,
        };
        let response = match body {
            Some((bytes, mime)) => Response::builder()
                .header(header::CONTENT_TYPE, mime)
                .header(header::CACHE_CONTROL, "max-age=3600")
                .body(bytes),
            None => Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Vec::new()),
        };
        responder.respond(response.expect("a valid response"));
    });
}

fn mime(path: &std::path::Path) -> &'static str {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "gif" => "image/gif",
        "png" => "image/png",
        "webp" => "image/webp",
        "jpg" | "jpeg" => "image/jpeg",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
}
