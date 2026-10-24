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
}

/// A wallpaper's mounted files, project and scene document.
pub struct WallpaperFiles {
    /// Stock assets, overlaid by the wallpaper directory, overlaid by its package.
    pub vfs: Vfs,
    pub project: Project,
    pub scene_file: String,
    pub scene: wpe_assets::Scene,
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
            let pkg = path.join("scene.pkg");
            (path.to_owned(), pkg.is_file().then_some(pkg), None)
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
        let scene_file = scene_file.unwrap_or_else(|| resolve_scene_file(&project, &vfs));
        let scene = load_json(&vfs, &scene_file)?;
        Ok(Self {
            vfs,
            project,
            scene_file,
            scene,
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
