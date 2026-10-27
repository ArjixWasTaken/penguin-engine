//! Serving a wallpaper directory as a web origin: paths match files
//! case-insensitively (pages written on Windows), nothing outside the
//! directory is reachable, and media get byte ranges.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub struct Response {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
}

impl Response {
    fn status(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }
}

/// The response to a GET of `path` (the URL path, percent-encoded) with
/// an optional `Range` header.
pub fn get(root: &Path, path: &str, range: Option<&str>) -> Response {
    match find(root, path) {
        Some(file) => respond(&file, range),
        None => Response::status(404),
    }
}

/// Largest single body buffered into RAM without a Range header (64 MiB).
const MAX_UNSTREAMED_BODY: u64 = 64 * 1024 * 1024;
/// Maximum bytes returned in a single Range response (32 MiB).
const MAX_RANGE_BYTES: u64 = 32 * 1024 * 1024;

/// The response to a GET of `file` with an optional `Range` header.
pub fn respond(file: &Path, range: Option<&str>) -> Response {
    let Ok(mut f) = std::fs::File::open(file) else {
        return Response::status(404);
    };
    let len = f.metadata().map_or(0, |m| m.len());
    let mime = mime(file);
    let mut headers = vec![
        ("Content-Type", mime.to_owned()),
        ("Accept-Ranges", "bytes".to_owned()),
        ("Cache-Control", "no-cache".to_owned()),
    ];
    match range.and_then(|r| parse_range(r, len)) {
        Some((start, mut end)) => {
            if end >= start && (end - start + 1) > MAX_RANGE_BYTES {
                end = start + MAX_RANGE_BYTES - 1;
            }
            let chunk_len = (end - start + 1) as usize;
            let mut body = vec![0; chunk_len];
            if f.seek(SeekFrom::Start(start)).is_err() || f.read_exact(&mut body).is_err() {
                return Response::status(500);
            }
            headers.push(("Content-Range", format!("bytes {start}-{end}/{len}")));
            headers.push(("Content-Length", body.len().to_string()));
            Response {
                status: 206,
                headers,
                body,
            }
        }
        None if range.is_some() => {
            let mut r = Response::status(416);
            r.headers.push(("Content-Range", format!("bytes */{len}")));
            r
        }
        None => {
            let read_len = len.min(MAX_UNSTREAMED_BODY) as usize;
            let mut body = Vec::with_capacity(read_len);
            use std::io::Read;
            if f.take(MAX_UNSTREAMED_BODY).read_to_end(&mut body).is_err() {
                return Response::status(500);
            }
            headers.push(("Content-Length", body.len().to_string()));
            Response {
                status: 200,
                headers,
                body,
            }
        }
    }
}

/// The file `path` names under `root`, matching each component
/// case-insensitively when there is no exact match. `..` is refused.
pub fn find(root: &Path, path: &str) -> Option<PathBuf> {
    let decoded = percent_decode(path)?;
    let mut at = root.to_owned();
    for part in decoded
        .split(['/', '\\'])
        .filter(|p| !p.is_empty() && *p != ".")
    {
        if part == ".." {
            return None;
        }
        let exact = at.join(part);
        at = if exact.exists() {
            exact
        } else {
            std::fs::read_dir(&at)
                .ok()?
                .flatten()
                .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(part))?
                .path()
        };
    }
    let at = if at.is_dir() {
        at.join("index.html")
    } else {
        at
    };
    at.is_file().then_some(at)
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// `bytes=a-b`, `bytes=a-` or `bytes=-n` of a `len`-byte file, as an
/// inclusive range. Only one range is served.
fn parse_range(header: &str, len: u64) -> Option<(u64, u64)> {
    let spec = header
        .trim()
        .strip_prefix("bytes=")?
        .split(',')
        .next()?
        .trim();
    let (a, b) = spec.split_once('-')?;
    let (start, end) = match (a.trim(), b.trim()) {
        ("", n) => {
            let n: u64 = n.parse().ok()?;
            (len.checked_sub(n.min(len))?, len.checked_sub(1)?)
        }
        (a, "") => (a.parse().ok()?, len.checked_sub(1)?),
        (a, b) => (
            a.parse().ok()?,
            b.parse::<u64>().ok()?.min(len.checked_sub(1)?),
        ),
    };
    (start <= end && start < len).then_some((start, end))
}

fn mime(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "txt" | "atlas" => "text/plain; charset=utf-8",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "ogv" => "video/ogg",
        "mp3" => "audio/mpeg",
        "ogg" | "oga" => "audio/ogg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "flac" => "audio/flac",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wpe-web-serve-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Image")).unwrap();
        std::fs::write(dir.join("index.html"), "<p>hi</p>").unwrap();
        std::fs::write(dir.join("Image/Logo.PNG"), b"0123456789").unwrap();
        dir
    }

    #[test]
    fn files_match_case_insensitively() {
        let root = dir();
        assert_eq!(
            find(&root, "/image/logo.png"),
            Some(root.join("Image/Logo.PNG"))
        );
        assert_eq!(find(&root, "/"), Some(root.join("index.html")));
        assert_eq!(
            find(&root, "/%49mage/Logo.PNG"),
            Some(root.join("Image/Logo.PNG"))
        );
        assert_eq!(find(&root, "/../etc/passwd"), None);
        assert_eq!(find(&root, "/image/%2e%2e/%2e%2e/x"), None);
        assert_eq!(find(&root, "/missing.js"), None);
    }

    #[test]
    fn ranges() {
        let root = dir();
        let r = get(&root, "/image/logo.png", Some("bytes=2-4"));
        assert_eq!((r.status, r.body.as_slice()), (206, &b"234"[..]));
        let r = get(&root, "/image/logo.png", Some("bytes=7-"));
        assert_eq!((r.status, r.body.as_slice()), (206, &b"789"[..]));
        let r = get(&root, "/image/logo.png", Some("bytes=-2"));
        assert_eq!((r.status, r.body.as_slice()), (206, &b"89"[..]));
        assert_eq!(get(&root, "/image/logo.png", Some("bytes=20-")).status, 416);
        let r = get(&root, "/index.html", None);
        assert_eq!((r.status, r.body.len()), (200, 9));
        assert!(
            r.headers
                .iter()
                .any(|(k, v)| *k == "Content-Type" && v.starts_with("text/html"))
        );
    }
}
