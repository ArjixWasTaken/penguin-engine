use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use wpe_assets::{AssetError, Project, load_json, resolve_scene_file};
use wpe_vfs::{AliasFs, FileSystem, PhysicalFs, PkgFs, Vfs, VfsError};

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error(transparent)]
    Vfs(#[from] VfsError),
    #[error(transparent)]
    Asset(#[from] AssetError),
    #[error("{0} is not a wallpaper directory, .pkg or scene .json")]
    BadPath(PathBuf),
    #[error("web wallpapers need a build with the `web` feature")]
    WebUnsupported,
    #[error("web page: {0}")]
    Web(String),
}

/// A wallpaper's mounted files, project and scene document.
pub struct WallpaperFiles {
    /// Stock assets, overlaid by the wallpaper directory, overlaid by its package.
    pub vfs: Vfs,
    pub project: Project,
    pub scene_file: String,
    pub scene: wpe_assets::Scene,
    /// The scene document as JSON, which scripts edit (`createLayer`, ...).
    pub scene_json: serde_json::Value,
    /// For a video wallpaper, its video and picture size; the scene is made
    /// up to show it (see [`crate::video_wallpaper`]).
    pub video: Option<(Vec<u8>, u32, u32)>,
    /// For a web wallpaper, its directory and page; the scene is made up
    /// to show it (see [`crate::web_wallpaper`]).
    pub web: Option<(PathBuf, String)>,
}

impl WallpaperFiles {
    /// Open the wallpaper at `path`: a directory with `project.json`, a
    /// `.pkg`, or a scene `.json`. `assets` is Wallpaper Engine's stock
    /// assets directory (shareable between wallpapers via `Arc`).
    pub fn open<F: FileSystem + Clone + 'static>(
        assets: F,
        path: &Path,
    ) -> Result<Self, LoadError> {
        let bad = || LoadError::BadPath(path.to_owned());
        let (dir, pkg, scene_file) = if path.is_dir() {
            // The package is named after the project's file (`gifscene.json`
            // ships in `gifscene.pkg`), usually `scene.pkg`.
            let project = std::fs::read_to_string(path.join("project.json"))
                .ok()
                .and_then(|text| wpe_assets::parse_json::<Project>(&text).ok());
            let stem = project
                .as_ref()
                .and_then(|p| p.file.as_deref())
                .and_then(|f| f.rsplit(['/', '\\']).next())
                .and_then(|f| f.rsplit_once('.').map(|(stem, _)| stem.to_owned()));
            let pkg = stem
                .map(|stem| path.join(format!("{stem}.pkg")))
                .into_iter()
                .chain([path.join("scene.pkg")])
                .find(|p| p.is_file());
            (path.to_owned(), pkg, None)
        } else {
            let dir = path.parent().ok_or_else(bad)?.to_owned();
            match path.extension().and_then(|e| e.to_str()) {
                Some("pkg") => (dir, Some(path.to_owned()), None),
                Some("json") => {
                    let name = path
                        .file_name()
                        .ok_or_else(bad)?
                        .to_string_lossy()
                        .into_owned();
                    (dir, None, Some(name))
                }
                _ => return Err(bad()),
            }
        };

        let mut vfs = Vfs::new();
        let aliases = effect_aliases(&assets);
        vfs.mount("stock effects", AliasFs::new(assets.clone(), aliases));
        vfs.mount("assets", assets);
        vfs.mount("wallpaper", PhysicalFs::new(&dir)?);
        if let Some(pkg) = &pkg {
            vfs.mount("pkg", PkgFs::open(pkg)?);
        }

        let project_path = dir.join("project.json");
        let project: Project = if project_path.is_file() {
            load_json(&vfs, "project.json")?
        } else {
            Project::default()
        };
        let mut video = None;
        if project.is_video()
            && scene_file.is_none()
            && let Some(file) = project.file.as_deref()
        {
            let mp4 = vfs.read(file)?.into_owned();
            let (width, height) = crate::video_wallpaper::dimensions(&mp4).unwrap_or((1920, 1080));
            vfs.mount(
                "video",
                crate::video_wallpaper::documents(crate::video_wallpaper::TEXTURE, width, height),
            );
            video = Some((mp4, width, height));
        }
        let mut web = None;
        if project.is_web() && scene_file.is_none() {
            let file = project
                .file
                .clone()
                .unwrap_or_else(|| "index.html".to_owned());
            let (width, height) = crate::web_wallpaper::DEFAULT_SIZE;
            vfs.mount(
                "web",
                crate::video_wallpaper::documents(crate::web_wallpaper::TEXTURE, width, height),
            );
            web = Some((dir.clone(), file));
        }
        let scene_file = match (&video, &web, scene_file) {
            (Some(_), _, _) | (_, Some(_), _) => crate::video_wallpaper::SCENE_FILE.to_owned(),
            (None, None, Some(file)) => file,
            (None, None, None) => resolve_scene_file(&project, &vfs),
        };
        let scene = load_json(&vfs, &scene_file)?;
        let scene_json = load_json(&vfs, &scene_file)?;
        Ok(Self {
            vfs,
            project,
            scene_file,
            scene,
            scene_json,
            video,
            web,
        })
    }

    /// [`Self::open`] with a freshly indexed assets directory.
    pub fn open_with_assets_dir(assets: &Path, path: &Path) -> Result<Self, LoadError> {
        Self::open(Arc::new(PhysicalFs::new(assets)?), path)
    }
}

/// Stock effects keep their materials and shaders inside the effect folder
/// (`effects/blur/shaders/effects/blur_downsample4.vert`) while materials
/// refer to them from the asset root (`shaders/effects/blur_downsample4`).
/// Map the root-relative names onto the first effect folder providing them.
fn effect_aliases(assets: &impl FileSystem) -> HashMap<String, String> {
    let mut paths: Vec<&str> = assets
        .files()
        .filter(|p| p.starts_with("effects/") && !p.contains("/preview/"))
        .collect();
    paths.sort_unstable();
    let mut aliases = HashMap::new();
    for path in paths {
        let mut parts = path.splitn(3, '/');
        let (Some(_), Some(_), Some(rest)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        if rest.starts_with("materials/") || rest.starts_with("shaders/") {
            aliases
                .entry(rest.to_owned())
                .or_insert_with(|| path.to_owned());
        }
    }
    aliases
}
