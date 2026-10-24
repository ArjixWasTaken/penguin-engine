//! What the app remembers between runs (`ui-state.json` in the config
//! directory): the search, filters, sort and selection.

use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use wpe_library::{Kind, Rating, Source};

use crate::app::{Filters, Sort};

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    pub query: String,
    pub kinds: Option<HashSet<Kind>>,
    pub ratings: Option<HashSet<Rating>>,
    pub sources: Option<HashSet<Source>>,
    pub audio_only: bool,
    pub sort: Sort,
    pub selected: Option<String>,
}

fn path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("wallpaper-engine-rs/ui-state.json"))
}

impl UiState {
    pub fn load() -> Self {
        path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = path() else { return };
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        let tmp = path.with_extension("json.tmp");
        let written = serde_json::to_vec_pretty(self)
            .map_err(std::io::Error::other)
            .and_then(|json| std::fs::write(&tmp, json))
            .and_then(|()| std::fs::rename(&tmp, &path));
        if let Err(e) = written {
            tracing::debug!("saving {}: {e}", path.display());
        }
    }

    pub fn filters(&self) -> Filters {
        let defaults = Filters::default();
        Filters {
            query: self.query.clone(),
            kinds: self.kinds.clone().unwrap_or(defaults.kinds),
            ratings: self.ratings.clone().unwrap_or(defaults.ratings),
            sources: self.sources.clone().unwrap_or(defaults.sources),
            audio_only: self.audio_only,
        }
    }

    pub fn of(filters: &Filters, sort: Sort, selected: Option<String>) -> Self {
        Self {
            query: filters.query.clone(),
            kinds: Some(filters.kinds.clone()),
            ratings: Some(filters.ratings.clone()),
            sources: Some(filters.sources.clone()),
            audio_only: filters.audio_only,
            sort,
            selected,
        }
    }
}
