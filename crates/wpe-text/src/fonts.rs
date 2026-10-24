use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use cosmic_text::fontdb::{self, Family, Query};
use cosmic_text::{Attrs, FontSystem};
use wpe_vfs::Vfs;

/// A face chosen as a text layer's primary font, with the metrics layout needs.
#[derive(Debug, Clone)]
pub(crate) struct Face {
    family: String,
    weight: fontdb::Weight,
    style: fontdb::Style,
    stretch: fontdb::Stretch,
    /// Ascender in em.
    pub ascent: f32,
    /// Ascender - descender + line gap, in em.
    pub line_height: f32,
}

impl Face {
    /// Attributes that make cosmic-text select this face.
    pub fn attrs(&self) -> Attrs<'_> {
        Attrs::new()
            .family(Family::Name(&self.family))
            .weight(self.weight)
            .style(self.style)
            .stretch(self.stretch)
    }
}

/// Fonts loaded from wallpapers, keyed by path and a content fingerprint so a
/// path reused by another wallpaper with a different file reloads.
#[derive(Default)]
pub(crate) struct FontCache {
    loaded: HashMap<(String, u64), Option<fontdb::ID>>,
}

/// Linux stand-ins for the Windows fonts `systemfont_<name>` refers to.
fn system_candidates(name: &str) -> &'static [&'static str] {
    match name {
        "arial" | "helvetica" | "sansserif" | "microsoftsansserif" | "tahoma" | "verdana" => &[
            "Arial",
            "Liberation Sans",
            "Arimo",
            "DejaVu Sans",
            "Noto Sans",
        ],
        "consolas" | "couriernew" | "courier" | "lucidaconsole" | "monospace" => &[
            "Consolas",
            "Liberation Mono",
            "Cousine",
            "DejaVu Sans Mono",
            "Noto Sans Mono",
        ],
        "timesnewroman" | "times" | "georgia" | "serif" => &[
            "Times New Roman",
            "Liberation Serif",
            "Tinos",
            "DejaVu Serif",
            "Noto Serif",
        ],
        "cambria" => &["Cambria", "Caladea", "Liberation Serif", "DejaVu Serif"],
        "calibri" => &["Calibri", "Carlito", "Liberation Sans", "DejaVu Sans"],
        "comicsans" | "comicsansms" => &["Comic Sans MS", "Comic Neue", "Comic Relief"],
        "segoeui" => &["Segoe UI", "Noto Sans", "Open Sans", "DejaVu Sans"],
        "impact" => &["Impact", "Anton", "Oswald"],
        _ => &[],
    }
}

const SANS_FALLBACK: &[&str] = &[
    "Liberation Sans",
    "Arimo",
    "DejaVu Sans",
    "Noto Sans",
    "Open Sans",
];

fn fingerprint(bytes: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    bytes.len().hash(&mut h);
    bytes[..bytes.len().min(4096)].hash(&mut h);
    bytes[bytes.len().saturating_sub(4096)..].hash(&mut h);
    h.finish()
}

fn query_family(db: &fontdb::Database, family: &str) -> Option<fontdb::ID> {
    db.query(&Query {
        families: &[Family::Name(family)],
        ..Query::default()
    })
    .filter(|id| {
        db.face(*id).is_some_and(|f| {
            f.families
                .iter()
                .any(|(n, _)| n.eq_ignore_ascii_case(family))
        })
    })
}

fn face_info(fs: &FontSystem, id: fontdb::ID) -> Option<Face> {
    let info = fs.db().face(id)?;
    let (ascent, line_height) = fs
        .db()
        .with_face_data(id, |data, index| {
            let f = ttf_parser::Face::parse(data, index).ok()?;
            let upm = f32::from(f.units_per_em().max(1));
            let asc = f32::from(f.ascender());
            let desc = f32::from(f.descender());
            let gap = f32::from(f.line_gap());
            (asc > desc).then(|| (asc / upm, (asc - desc + gap.max(0.0)) / upm))
        })
        .flatten()
        .unwrap_or((0.9, 1.2));
    Some(Face {
        family: info.families.first()?.0.clone(),
        weight: info.weight,
        style: info.style,
        stretch: info.stretch,
        ascent,
        line_height,
    })
}

impl FontCache {
    /// The primary face for `font`: a file in `vfs`, a `systemfont_*` alias,
    /// or (when neither resolves) a common installed sans-serif.
    pub fn resolve(&mut self, fs: &mut FontSystem, vfs: &Vfs, font: &str) -> Option<Face> {
        let id = self
            .load(fs, vfs, font)
            .or_else(|| {
                if !font.is_empty() {
                    tracing::warn!(font, "text font unavailable; using a system sans-serif");
                }
                SANS_FALLBACK.iter().find_map(|f| query_family(fs.db(), f))
            })
            .or_else(|| {
                fs.db().query(&Query {
                    families: &[Family::SansSerif],
                    ..Query::default()
                })
            })
            .or_else(|| fs.db().faces().next().map(|f| f.id))?;
        face_info(fs, id)
    }

    fn load(&mut self, fs: &mut FontSystem, vfs: &Vfs, font: &str) -> Option<fontdb::ID> {
        let font = font.trim();
        if let Some(name) = font.strip_prefix("systemfont_") {
            let name = name.to_ascii_lowercase().replace([' ', '-', '_'], "");
            return system_candidates(&name)
                .iter()
                .copied()
                .chain(std::iter::once(font.trim_start_matches("systemfont_")))
                .find_map(|f| query_family(fs.db(), f));
        }
        if font.is_empty() {
            return None;
        }
        let bytes = ["", "assets/"]
            .iter()
            .find_map(|p| vfs.read(&format!("{p}{font}")).ok())?;
        let key = (wpe_vfs::normalize(font), fingerprint(&bytes));
        if let Some(id) = self.loaded.get(&key) {
            return *id;
        }
        let id = fs
            .db_mut()
            .load_font_source(fontdb::Source::Binary(Arc::new(bytes.into_owned())))
            .first()
            .copied();
        if id.is_none() {
            tracing::warn!(font, "text font is not a font file fontdb accepts");
        }
        self.loaded.insert(key, id);
        id
    }
}
