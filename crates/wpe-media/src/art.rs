//! Album art: `mpris:artUrl` parsing, fetching and decoding.

use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use crate::MediaError;
use crate::colors::extract_colors;
use crate::event::ThumbnailColors;

/// Longest side of a decoded [`Thumbnail`]; larger art is downscaled.
pub const MAX_THUMBNAIL_SIDE: u32 = 512;
/// Largest encoded art accepted from a file or HTTP response.
pub const MAX_ART_BYTES: u64 = 32 * 1024 * 1024;
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// Decoded album art plus its derived colours.
#[derive(Debug, Clone, PartialEq)]
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGBA8, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
    pub colors: ThumbnailColors,
    /// Increases every time the monitor publishes new art (starts at 1).
    pub generation: u64,
}

/// Where `mpris:artUrl` points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtSource {
    File(PathBuf),
    Http(String),
    Data(Vec<u8>),
}

/// Parses an `mpris:artUrl`. `file://` paths are percent-decoded; a bare
/// absolute path is accepted too (some players send one).
pub fn parse_art_url(url: &str) -> Result<ArtSource, MediaError> {
    let url = url.trim();
    let bad = |why: &str| MediaError::Art(format!("{why}: {}", truncate(url)));
    let lower_prefix = |p: &str| url.len() >= p.len() && url[..p.len()].eq_ignore_ascii_case(p);

    if lower_prefix("file://") {
        let rest = &url["file://".len()..];
        // file://host/path: only an empty host or "localhost" is local.
        let path = if rest.starts_with('/') {
            rest
        } else if let Some(p) = rest.strip_prefix("localhost") {
            p
        } else {
            return Err(bad("non-local file URL"));
        };
        let path = path.split(['?', '#']).next().unwrap_or(path);
        let bytes: Vec<u8> = percent_encoding::percent_decode_str(path).collect();
        use std::os::unix::ffi::OsStringExt;
        Ok(ArtSource::File(PathBuf::from(
            std::ffi::OsString::from_vec(bytes),
        )))
    } else if lower_prefix("http://") || lower_prefix("https://") {
        Ok(ArtSource::Http(url.to_string()))
    } else if lower_prefix("data:") {
        let du = data_url::DataUrl::process(url).map_err(|_| bad("malformed data URL"))?;
        let (body, _) = du
            .decode_to_vec()
            .map_err(|_| bad("bad data URL payload"))?;
        Ok(ArtSource::Data(body))
    } else if url.starts_with('/') {
        Ok(ArtSource::File(PathBuf::from(url)))
    } else {
        Err(bad("unsupported art URL"))
    }
}

fn truncate(s: &str) -> &str {
    match s.char_indices().nth(80) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

fn is_safe_http_url(url_str: &str) -> bool {
    use std::net::ToSocketAddrs;
    let Ok(url) = url::Url::parse(url_str) else {
        return false;
    };
    if url.scheme() != "http" && url.scheme() != "https" {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        return false;
    }
    let port = url.port_or_known_default().unwrap_or(80);
    let Ok(addrs) = (host, port).to_socket_addrs() else {
        return false;
    };
    let mut any = false;
    for addr in addrs {
        any = true;
        let is_local = match addr.ip() {
            std::net::IpAddr::V4(v4) => {
                v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
            }
            std::net::IpAddr::V6(v6) => {
                v6.is_loopback()
                    || v6.is_unspecified()
                    || (v6.segments()[0] & 0xfe00 == 0xfc00)
                    || (v6.segments()[0] & 0xffc0 == 0xfe80)
            }
        };
        if is_local {
            return false;
        }
    }
    any
}

/// Reads the encoded bytes behind an [`ArtSource`]. Blocking.
pub fn fetch(source: &ArtSource) -> Result<Vec<u8>, MediaError> {
    match source {
        ArtSource::Data(b) => Ok(b.clone()),
        ArtSource::File(p) => {
            let f = std::fs::File::open(p)
                .map_err(|e| MediaError::Art(format!("{}: {e}", p.display())))?;
            let mut buf = Vec::new();
            f.take(MAX_ART_BYTES)
                .read_to_end(&mut buf)
                .map_err(|e| MediaError::Art(format!("{}: {e}", p.display())))?;
            Ok(buf)
        }
        ArtSource::Http(url) => {
            if !is_safe_http_url(url) {
                return Err(MediaError::Art(format!(
                    "blocked request to local or invalid URL: {}",
                    truncate(url)
                )));
            }
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .timeout_global(Some(HTTP_TIMEOUT))
                .build()
                .into();
            let mut resp = agent
                .get(url)
                .call()
                .map_err(|e| MediaError::Art(format!("{}: {e}", truncate(url))))?;
            resp.body_mut()
                .with_config()
                .limit(MAX_ART_BYTES)
                .read_to_vec()
                .map_err(|e| MediaError::Art(format!("{}: {e}", truncate(url))))
        }
    }
}

/// Decodes encoded art, downscales it so the longest side is at most
/// [`MAX_THUMBNAIL_SIDE`] and computes its colours. `generation` is left 0.
pub fn decode(bytes: &[u8]) -> Result<Thumbnail, MediaError> {
    let img = image::load_from_memory(bytes)
        .map_err(|e| MediaError::Art(format!("decode: {e}")))?
        .into_rgba8();
    let (w, h) = img.dimensions();
    let img = if w.max(h) > MAX_THUMBNAIL_SIDE {
        let scale = MAX_THUMBNAIL_SIDE as f32 / w.max(h) as f32;
        let nw = ((w as f32 * scale).round() as u32).clamp(1, MAX_THUMBNAIL_SIDE);
        let nh = ((h as f32 * scale).round() as u32).clamp(1, MAX_THUMBNAIL_SIDE);
        image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let (width, height) = img.dimensions();
    let rgba = img.into_raw();
    let colors = extract_colors(width, height, &rgba);
    Ok(Thumbnail {
        width,
        height,
        rgba,
        colors,
        generation: 0,
    })
}

/// [`parse_art_url`] + [`fetch`] + [`decode`].
pub fn load(url: &str) -> Result<Thumbnail, MediaError> {
    decode(&fetch(&parse_art_url(url)?)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32, c: [u8; 4]) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba(c));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn file_urls_are_percent_decoded() {
        assert_eq!(
            parse_art_url("file:///home/u/My%20Music/caf%C3%A9%20art.jpg").unwrap(),
            ArtSource::File("/home/u/My Music/café art.jpg".into())
        );
        assert_eq!(
            parse_art_url("file://localhost/tmp/a.png").unwrap(),
            ArtSource::File("/tmp/a.png".into())
        );
        assert_eq!(
            parse_art_url("FILE:///tmp/x%23y.png").unwrap(),
            ArtSource::File("/tmp/x#y.png".into())
        );
        assert_eq!(
            parse_art_url("/tmp/plain.png").unwrap(),
            ArtSource::File("/tmp/plain.png".into())
        );
        assert!(parse_art_url("file://otherhost/x.png").is_err());
    }

    #[test]
    fn http_and_unknown_schemes() {
        assert_eq!(
            parse_art_url("https://i.scdn.co/image/ab67").unwrap(),
            ArtSource::Http("https://i.scdn.co/image/ab67".into())
        );
        assert!(parse_art_url("").is_err());
        assert!(parse_art_url("ftp://x/y").is_err());
    }

    #[test]
    fn data_urls_base64_and_plain() {
        assert_eq!(
            parse_art_url("data:image/png;base64,AAEC/w==").unwrap(),
            ArtSource::Data(vec![0, 1, 2, 255])
        );
        assert_eq!(
            parse_art_url("data:,hi%20there").unwrap(),
            ArtSource::Data(b"hi there".to_vec())
        );
        assert!(parse_art_url("data:image/png;base64,@@@").is_err());
    }

    #[test]
    fn decode_downscales_to_cap() {
        let t = decode(&png(1024, 256, [0, 128, 255, 255])).unwrap();
        assert_eq!((t.width, t.height), (512, 128));
        assert_eq!(t.rgba.len(), 512 * 128 * 4);
        assert_eq!(&t.rgba[..4], &[0, 128, 255, 255]);
        let small = decode(&png(3, 2, [1, 2, 3, 255])).unwrap();
        assert_eq!((small.width, small.height), (3, 2));
        assert!(decode(b"not an image").is_err());
    }

    #[test]
    fn load_file_and_data_end_to_end() {
        let bytes = png(4, 4, [255, 0, 0, 255]);
        let dir = std::env::temp_dir().join(format!("wpe-media-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("art work.png");
        std::fs::write(&path, &bytes).unwrap();
        let url = format!("file://{}", path.display().to_string().replace(' ', "%20"));
        let t = load(&url).unwrap();
        assert_eq!((t.width, t.height), (4, 4));
        assert_eq!(t.colors.primary, [1.0, 0.0, 0.0]);
        std::fs::remove_dir_all(&dir).ok();

        use std::fmt::Write;
        let mut b64 = String::new();
        // Tiny base64 encoder keeps the test free of extra dev-dependencies.
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        for chunk in bytes.chunks(3) {
            let n = chunk.len();
            let v = (chunk[0] as u32) << 16
                | (*chunk.get(1).unwrap_or(&0) as u32) << 8
                | *chunk.get(2).unwrap_or(&0) as u32;
            for i in 0..4 {
                if i <= n {
                    b64.write_char(T[(v >> (18 - 6 * i) & 63) as usize] as char)
                        .unwrap();
                } else {
                    b64.push('=');
                }
            }
        }
        let t = load(&format!("data:image/png;base64,{b64}")).unwrap();
        assert_eq!((t.width, t.height), (4, 4));
    }

    #[test]
    fn blocks_unsafe_http_urls() {
        assert!(!is_safe_http_url("http://127.0.0.1/test"));
        assert!(!is_safe_http_url("http://localhost:8080/secret"));
        assert!(!is_safe_http_url("http://169.254.169.254/latest/meta-data"));
        assert!(!is_safe_http_url("http://192.168.1.1/admin"));
        assert!(!is_safe_http_url("http://10.0.0.1/internal"));
        assert!(!is_safe_http_url("http://[::1]/test"));
        assert!(!is_safe_http_url("ftp://example.com/file"));
        assert!(!is_safe_http_url("http://invalid.domain.that.cannot.resolve.penguin/"));
    }
}
