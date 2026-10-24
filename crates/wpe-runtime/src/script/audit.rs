//! `WPE_SCRIPT_AUDIT=<file>`: counts of the script requests the host doesn't
//! handle and of script errors, written to the file as JSON when the
//! wallpaper unloads. Finds what scripts need that isn't implemented yet.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub struct Audit {
    path: PathBuf,
    counts: RefCell<BTreeMap<String, u32>>,
}

impl Audit {
    pub fn from_env() -> Option<Self> {
        let path = std::env::var_os("WPE_SCRIPT_AUDIT")?;
        Some(Self {
            path: path.into(),
            counts: Default::default(),
        })
    }

    pub fn count(&self, kind: &str, what: &str) {
        *self
            .counts
            .borrow_mut()
            .entry(format!("{kind} {}", key(what)))
            .or_default() += 1;
    }
}

impl Drop for Audit {
    fn drop(&mut self) {
        let json = serde_json::to_string(&*self.counts.borrow()).unwrap_or_default();
        if let Err(e) = std::fs::write(&self.path, json) {
            tracing::warn!("writing the script audit: {e}");
        }
    }
}

/// `what` with numbers and animation names generalised, so requests group.
fn key(what: &str) -> String {
    let mut key = String::new();
    let mut digits = false;
    for c in what.chars() {
        if c.is_ascii_digit() {
            if !digits {
                key.push('#');
            }
            digits = true;
        } else {
            digits = false;
            key.push(c);
        }
    }
    for prefix in ["animation:", "animationLayer:"] {
        if let Some(at) = key.find(prefix) {
            let end = key[at..].find('.').map_or(key.len(), |e| at + e);
            key.replace_range(at + prefix.len()..end, "*");
        }
    }
    key
}
