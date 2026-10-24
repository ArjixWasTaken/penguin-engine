//! Loading the library and Wallpaper Engine's UI strings off the UI thread.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use wpe_ipc::PropertyInfo;
use wpe_library::locale::{self, Locale};
use wpe_library::{Item, Library, Sources, steam};

/// A wallpaper's properties and their labels.
pub type Props = (Vec<PropertyInfo>, HashMap<String, String>);

#[derive(Clone)]
pub struct Loaded {
    pub items: Arc<Vec<Item>>,
    pub locale: Arc<Locale>,
}

impl std::fmt::Debug for Loaded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Loaded({} items)", self.items.len())
    }
}

pub async fn load() -> Loaded {
    tokio::task::spawn_blocking(|| {
        let sources = Sources::discover(&[]);
        let library = match wpe_library::default_cache_path() {
            Some(cache) => Library::load_or_scan(&sources, &cache),
            None => Library::scan(&sources),
        };
        let lang = locale::env_lang();
        let locale = match steam::we_dir() {
            Some(dir) => Locale::load(&dir, &lang),
            None => Locale::from_strings(&lang, HashMap::new()),
        };
        Loaded {
            items: Arc::new(library.items),
            locale: Arc::new(locale),
        }
    })
    .await
    .expect("loading the library")
}

/// Property labels in the user's language, as plain text.
pub fn labels(locale: &Locale, dir: PathBuf, props: &[PropertyInfo]) -> HashMap<String, String> {
    let localization = wpe_library::localization(&dir);
    props
        .iter()
        .map(|p| {
            let text = p.text.as_deref().unwrap_or(&p.name);
            let label = locale::plain(&locale.label(text, localization.as_ref()));
            let label = if label.trim().is_empty() {
                p.name.clone()
            } else {
                label
            };
            (p.name.clone(), label)
        })
        .collect()
}

/// The properties in a wallpaper's project, for one that isn't shown.
pub async fn properties(dir: PathBuf) -> Vec<PropertyInfo> {
    tokio::task::spawn_blocking(move || wpe_library::properties(&dir))
        .await
        .unwrap_or_default()
}
