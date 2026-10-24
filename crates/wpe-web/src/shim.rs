//! The user script giving pages Wallpaper Engine's JavaScript API
//! (`wallpaperPropertyListener`, `wallpaperRegisterAudioListener`, ...).

const SOURCE: &str = include_str!("shim.js");

/// The shim with the page's starting state.
pub fn script(properties: &serde_json::Value, general: &serde_json::Value) -> String {
    let init = serde_json::json!({
        "properties": properties,
        "general": general,
        "paused": false,
    });
    SOURCE.replace("__WPE_INIT__", &init.to_string())
}
