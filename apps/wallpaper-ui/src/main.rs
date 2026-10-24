//! The settings app: browse the installed wallpapers, apply them and
//! change their properties. A client of the `wallpaper` daemon.

mod app;
mod color_picker;
mod daemon;
mod icons;
mod library;
mod properties;
mod state;
mod theme;
mod thumbs;
mod view;

fn main() -> iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "info,wgpu_core=warn,wgpu_hal=warn,iced_wgpu=warn,naga=warn".into()
            }),
        )
        .init();

    iced::application(app::App::new, app::App::update, app::App::view)
        .title(app::App::title)
        .theme(app::App::theme)
        .subscription(app::App::subscription)
        .window_size((1400.0, 860.0))
        .run()
}
