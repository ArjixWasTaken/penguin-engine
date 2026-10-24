//! The settings app: a web page (`ui/`) in a Tauri window, over the
//! library ([`wpe_library`]) and `penguind` ([`wpe_ipc`]).
//! What the page can call and hear is listed in `API.md`.

mod commands;
mod events;
mod media;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,mp4parse=error".into()),
        )
        .init();
    wpe_library::migrate_dirs();

    let result = tauri::Builder::default()
        .runtime(tauri_runtime_wry::Wry::default())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(commands::Library::default())
        .register_asynchronous_uri_scheme_protocol("thumb", media::thumb)
        .register_asynchronous_uri_scheme_protocol("preview", media::preview)
        .invoke_handler(tauri::generate_handler![
            commands::library,
            commands::daemon,
            commands::properties,
            commands::conditions,
            commands::resolution,
            commands::resolutions,
            commands::folders,
            commands::save_folders,
            commands::autostart,
            commands::set_autostart,
            commands::presets,
            commands::save_preset,
            commands::delete_preset,
            commands::ui_state,
            commands::save_ui_state,
            commands::open_workshop,
            commands::open_folder,
        ])
        .setup(|app| {
            events::follow(app.handle().clone());
            Ok(())
        })
        .run(tauri::generate_context!());
    if let Err(e) = result {
        eprintln!("penguin-engine: {e}");
        std::process::exit(1);
    }
}
