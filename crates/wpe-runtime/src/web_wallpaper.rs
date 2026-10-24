//! Web wallpapers (project type `web`): the page, rendered by Servo
//! (`wpe-web`), shown over the whole output. Like a video wallpaper it is a
//! made-up one-layer scene, whose texture takes the page's frames; the page
//! is laid out at the output's size.

#[cfg(feature = "web")]
pub use enabled::*;

/// Texture name the page paints into.
pub const TEXTURE: &str = "__web";
/// The page's size until the wallpaper is first rendered.
pub const DEFAULT_SIZE: (u32, u32) = (1920, 1080);

/// User properties as pages get them, `{name: {type, value}}`.
pub fn properties_json(project: &wpe_assets::Project) -> serde_json::Value {
    project
        .general
        .properties
        .iter()
        .map(|(name, p)| (name.clone(), property_json(p.kind.as_deref(), &p.value)))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

pub fn property_json(kind: Option<&str>, value: &serde_json::Value) -> serde_json::Value {
    let mut entry = serde_json::json!({ "value": value });
    if let Some(kind) = kind {
        entry["type"] = kind.into();
    }
    entry
}

#[cfg(feature = "web")]
mod enabled {
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use wpe_render::{Gpu, GpuTexture};
    pub use wpe_web::{Button, NetworkPolicy, shutdown};

    use crate::LoadError;

    static NETWORK: Mutex<Option<NetworkPolicy>> = Mutex::new(None);

    /// Hosts web wallpapers may reach although they are local (see
    /// [`NetworkPolicy`]); applies to pages opened afterwards.
    pub fn set_network_policy(policy: NetworkPolicy) {
        *NETWORK.lock().unwrap() = Some(policy);
    }

    fn network_policy() -> NetworkPolicy {
        if let Some(policy) = NETWORK.lock().unwrap().clone() {
            return policy;
        }
        // `WPE_WEB_ALLOW_HOSTS=nas.lan,192.168.1.10` for tools without settings.
        NetworkPolicy {
            allowed_hosts: std::env::var("WPE_WEB_ALLOW_HOSTS")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|h| !h.is_empty())
                .map(ToOwned::to_owned)
                .collect(),
        }
    }

    /// A running web wallpaper's page and the texture showing it.
    pub struct WebState {
        pub page: wpe_web::WebPage,
        pub target: Option<Arc<GpuTexture>>,
        /// Size of the frame the target shows.
        pub shown: Option<(u32, u32)>,
    }

    /// Open the page `file` of the wallpaper in `dir`.
    pub fn open(
        dir: &Path,
        file: &str,
        project: &wpe_assets::Project,
        size: (u32, u32),
    ) -> Result<wpe_web::WebPage, LoadError> {
        let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_owned());
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "page".to_owned());
        let options = wpe_web::PageOptions {
            root: dir,
            file: file.to_owned(),
            name,
            size,
            scale: 1.0,
            properties: super::properties_json(project),
            general: serde_json::json!({ "fps": 60 }),
            network: network_policy(),
            profile_dir: dirs::data_dir().map(|d| d.join("wallpaper-engine-rs/web")),
        };
        wpe_web::WebPage::open(options, Duration::from_secs(20))
            .map_err(|e| LoadError::Web(e.to_string()))
    }

    impl WebState {
        /// Show the page's newest frame, if it painted one.
        pub fn upload(&mut self, gpu: &Gpu) {
            let (Some(target), Some(frame)) = (&self.target, self.page.take_frame()) else {
                return;
            };
            let size = target.texture.size();
            // Frames painted before a resize don't fit.
            if (frame.width, frame.height) == (size.width, size.height) {
                target.write_rgba(gpu, &frame.pixels);
                self.shown = Some((frame.width, frame.height));
            }
        }

        /// [`Self::upload`], first waiting up to `timeout` for a frame at
        /// the page's current size if none is shown yet (for screenshots).
        pub fn wait_frame(&mut self, gpu: &Gpu, timeout: Duration) {
            let deadline = std::time::Instant::now() + timeout;
            loop {
                self.upload(gpu);
                if self.shown == Some(self.page.size()) {
                    return;
                }
                if std::time::Instant::now() >= deadline {
                    tracing::warn!("no frame from the page within {timeout:?}");
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        /// The output pointer (0..1, Y down) in page pixels.
        pub fn page_point(&self, pointer: glam::Vec2) -> (f32, f32) {
            let (w, h) = self.page.size();
            (pointer.x * w as f32, pointer.y * h as f32)
        }
    }
}
