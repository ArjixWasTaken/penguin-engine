//! Lucide icons (ISC, `icons/LICENSE`), drawn in the text colour.

use iced::widget::svg::{self, Svg};
use iced::{Element, Theme};

macro_rules! icons {
    ($($name:ident = $file:literal),* $(,)?) => {
        $(pub fn $name<'a, Message: 'a>(size: f32) -> Element<'a, Message> {
            icon(include_bytes!(concat!("../icons/", $file, ".svg")), size)
        })*
    };
}

icons! {
    pause = "pause",
    play = "play",
    folder = "folder-open",
    external = "external-link",
    reset = "rotate-ccw",
    monitor = "monitor",
}

fn icon<'a, Message: 'a>(bytes: &'static [u8], size: f32) -> Element<'a, Message> {
    Svg::new(svg::Handle::from_memory(bytes))
        .width(size)
        .height(size)
        .style(|theme: &Theme, _| svg::Style {
            color: Some(theme.palette().text),
        })
        .into()
}
