//! What the app looks like.

use iced::widget::{
    Space, button, checkbox, column, container, grid, image, mouse_area, pick_list, row,
    scrollable, sensor, space, stack, text, text_input,
};
use iced::{Alignment, Color, ContentFit, Element, Fill, Length};
use wpe_library::{Item, Kind, Rating, Source};

use crate::app::{App, DaemonState, Message, Sort, Target};
use crate::{icons, theme};

const SIDEBAR: f32 = 190.0;
const DETAIL: f32 = 360.0;
const TILE: f32 = 250.0;

pub fn view(app: &App) -> Element<'_, Message> {
    let mut body = column![top_bar(app)];
    if let Some(banner) = banner(app) {
        body = body.push(container(banner).padding([8, 12]));
    }
    body.push(row![filters(app), library(app), detail(app)].height(Fill))
        .into()
}

fn top_bar(app: &App) -> Element<'_, Message> {
    let search = text_input("Search titles and tags", &app.filters.query)
        .on_input(Message::Query)
        .padding([7, 10])
        .width(Length::Fixed(340.0))
        .style(theme::search);
    let sort = pick_list(Sort::ALL, Some(app.sort), Message::Sort).padding([6, 10]);

    let mut targets = vec![Target::All];
    if let Some(status) = app.status() {
        targets.extend(status.outputs.iter().map(|o| Target::Output(o.name.clone())));
    }
    let target = pick_list(targets, Some(app.target.clone()), Message::Target).padding([6, 10]);

    let paused = app.status().is_some_and(|s| {
        let mut outputs = s.outputs.iter().filter(|o| match &app.target {
            Target::All => true,
            Target::Output(name) => &o.name == name,
        });
        outputs.clone().next().is_some() && outputs.all(|o| o.paused)
    });
    let pause = button(
        row![
            if paused { icons::play(14.0) } else { icons::pause(14.0) },
            text(if paused { "Resume" } else { "Pause" }).size(14)
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    )
        .padding([6, 12])
        .style(button::secondary)
        .on_press_maybe(app.status().map(|_| Message::Pause(!paused)));

    let bar = row![
        text("Wallpapers").size(18),
        Space::new().width(24),
        search,
        Space::new().width(8),
        text("Sort").size(13).color(theme::MUTED),
        sort,
        space::horizontal(),
        icons::monitor(16.0),
        target,
        pause,
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    container(bar)
        .padding([10, 14])
        .width(Fill)
        .style(theme::top_bar)
        .into()
}

fn banner(app: &App) -> Option<Element<'_, Message>> {
    if let Some(notice) = &app.notice {
        let color = if notice.error { theme::DANGER } else { theme::SUCCESS };
        let content = row![
            text(&notice.text).size(14).width(Fill),
            button(text("Dismiss").size(13))
                .style(button::text)
                .on_press(Message::DismissNotice),
        ]
        .align_y(Alignment::Center);
        return Some(
            container(content)
                .padding([6, 12])
                .width(Fill)
                .style(theme::banner(color))
                .into(),
        );
    }
    let DaemonState::Down(error) = &app.daemon else {
        return None;
    };
    let content = row![
        text(format!("Not connected: {error}. Wallpapers can't be applied until it runs."))
            .size(14)
            .width(Fill),
        button(text("Retry").size(13))
            .style(button::secondary)
            .on_press(Message::RefreshStatus),
    ]
    .align_y(Alignment::Center)
    .spacing(8);
    Some(
        container(content)
            .padding([6, 12])
            .width(Fill)
            .style(theme::banner(theme::WARNING))
            .into(),
    )
}

fn heading(label: &str) -> Element<'_, Message> {
    text(label.to_uppercase())
        .size(11)
        .color(theme::MUTED)
        .into()
}

fn filters(app: &App) -> Element<'_, Message> {
    let f = &app.filters;
    let kinds = [
        (Kind::Scene, "Scene"),
        (Kind::Video, "Video"),
        (Kind::Web, "Web"),
        (Kind::Preset, "Preset"),
        (Kind::Application, "Application"),
    ]
    .into_iter()
    .map(|(kind, label)| {
        checkbox(f.kinds.contains(&kind))
            .label(label)
            .size(16)
            .text_size(14)
            .on_toggle(move |_| Message::ToggleKind(kind))
            .into()
    });
    let ratings = [
        (Rating::Everyone, "Everyone"),
        (Rating::Questionable, "Questionable"),
        (Rating::Mature, "Mature"),
    ]
    .into_iter()
    .map(|(rating, label)| {
        checkbox(f.ratings.contains(&rating))
            .label(label)
            .size(16)
            .text_size(14)
            .on_toggle(move |_| Message::ToggleRating(rating))
            .into()
    });
    let sources = [
        (Source::Workshop, "Workshop"),
        (Source::Projects, "Wallpaper Engine"),
        (Source::User, "My folders"),
    ]
    .into_iter()
    .map(|(source, label)| {
        checkbox(f.sources.contains(&source))
            .label(label)
            .size(16)
            .text_size(14)
            .on_toggle(move |_| Message::ToggleSource(source))
            .into()
    });

    let count = if app.loading {
        "Loading…".to_owned()
    } else {
        format!("{} of {}", app.visible.len(), app.items.len())
    };
    let content = column![
        heading("Type"),
        column(kinds).spacing(6),
        Space::new().height(10),
        heading("Age rating"),
        column(ratings).spacing(6),
        Space::new().height(10),
        heading("Source"),
        column(sources).spacing(6),
        Space::new().height(10),
        heading("Other"),
        checkbox(f.audio_only)
            .label("Audio responsive")
            .size(16)
            .text_size(14)
            .on_toggle(Message::AudioOnly),
        Space::new().height(14),
        button(text("Reset filters").size(13))
            .style(button::secondary)
            .on_press(Message::ClearFilters),
        space::vertical(),
        text(count).size(12).color(theme::MUTED),
    ]
    .spacing(6);
    container(content)
        .padding(14)
        .width(SIDEBAR)
        .height(Fill)
        .style(theme::panel)
        .into()
}

fn library(app: &App) -> Element<'_, Message> {
    if app.visible.is_empty() {
        let message = if app.loading {
            "Loading wallpapers…"
        } else if app.items.is_empty() {
            "No wallpapers found. Subscribe to some on the Steam workshop."
        } else {
            "Nothing matches the filters."
        };
        return container(text(message).color(theme::MUTED))
            .center(Fill)
            .into();
    }
    let tiles = app.visible.iter().map(|&i| tile(app, &app.items[i]));
    let grid = grid(tiles)
        .fluid(TILE)
        .height(grid::aspect_ratio(16, 9))
        .spacing(10);
    scrollable(container(grid).padding(12))
        .height(Fill)
        .width(Fill)
        .into()
}

fn tile<'a>(app: &'a App, item: &'a Item) -> Element<'a, Message> {
    let id = &item.id;
    let selected = app.selected.as_deref() == Some(id);
    let hovered = app.hovered.as_deref() == Some(id);

    let picture: Element<'_, Message> = match app.thumbs.get(id) {
        Some(handle) => image(handle.clone())
            .width(Fill)
            .height(Fill)
            .content_fit(ContentFit::Cover)
            .border_radius(theme::RADIUS)
            .into(),
        None => space().width(Fill).height(Fill).into(),
    };

    let mut badges = row![].spacing(4);
    if let Some((label, color)) = kind_badge(item.kind) {
        badges = badges.push(badge(label, color));
    }
    if let Some((label, color)) = rating_badge(item.rating) {
        badges = badges.push(badge(label, color));
    }
    let title = container(
        text(&item.title)
            .size(13)
            .wrapping(text::Wrapping::None),
    )
    .padding([14, 8])
    .width(Fill)
    .clip(true)
    .style(theme::title_shade);
    let overlay = column![
        row![space::horizontal(), badges].padding(6),
        space::vertical(),
        title,
    ];
    let frame = container(space().width(Fill).height(Fill))
        .style(move |_| {
            let mut style = theme::tile(selected, hovered);
            style.background = None;
            style
        });
    let base = container(space().width(Fill).height(Fill)).style(|_| theme::tile(false, false));

    let card = mouse_area(stack![base, picture, overlay, frame])
        .on_press(Message::Select(id.clone()))
        .on_double_click(Message::Apply(id.clone()))
        .on_enter(Message::Hover(id.clone(), true))
        .on_exit(Message::Hover(id.clone(), false))
        .interaction(iced::mouse::Interaction::Pointer);
    sensor(card)
        .key_ref(id.as_str())
        .anticipate(300)
        .on_show(move |_| Message::TileShown(id.clone()))
        .into()
}

fn kind_badge(kind: Kind) -> Option<(&'static str, Color)> {
    match kind {
        Kind::Video => Some(("VIDEO", Color::from_rgb8(0x8b, 0x5c, 0xf6))),
        Kind::Web => Some(("WEB", Color::from_rgb8(0x0e, 0xa5, 0xe9))),
        Kind::Preset => Some(("PRESET", Color::from_rgb8(0x64, 0x74, 0x8b))),
        Kind::Application => Some(("APP", Color::from_rgb8(0x64, 0x74, 0x8b))),
        Kind::Scene | Kind::Unknown => None,
    }
}

fn rating_badge(rating: Rating) -> Option<(&'static str, Color)> {
    match rating {
        Rating::Questionable => Some(("Q", theme::WARNING)),
        Rating::Mature => Some(("M", theme::DANGER)),
        _ => None,
    }
}

fn badge(label: &str, color: Color) -> Element<'_, Message> {
    container(text(label).size(10))
        .padding([2, 5])
        .style(theme::badge(color))
        .into()
}

fn detail(app: &App) -> Element<'_, Message> {
    let Some(item) = app.selected.as_deref().and_then(|id| app.item(id)) else {
        return container(text("Select a wallpaper").color(theme::MUTED))
            .center_x(DETAIL)
            .center_y(Fill)
            .style(theme::panel)
            .into();
    };

    let preview: Element<'_, Message> = match app
        .preview
        .as_ref()
        .filter(|(id, _)| *id == item.id)
        .map(|(_, h)| h)
        .or_else(|| app.thumbs.get(&item.id))
    {
        Some(handle) => image(handle.clone())
            .width(Fill)
            .content_fit(ContentFit::Contain)
            .border_radius(theme::RADIUS)
            .into(),
        None => container(space())
            .width(Fill)
            .height(180)
            .style(|_| theme::tile(false, false))
            .into(),
    };

    let mut badges = row![].spacing(4);
    let kind = match item.kind {
        Kind::Scene | Kind::Unknown => ("SCENE", Color::from_rgb8(0x3f, 0xb9, 0x50)),
        k => kind_badge(k).unwrap(),
    };
    badges = badges.push(badge(kind.0, kind.1));
    match item.rating {
        Rating::Everyone => badges = badges.push(badge("EVERYONE", Color::from_rgb8(0x3f, 0x8f, 0x50))),
        Rating::Questionable => badges = badges.push(badge("QUESTIONABLE", theme::WARNING)),
        Rating::Mature => badges = badges.push(badge("MATURE", theme::DANGER)),
        Rating::Unknown => {}
    }
    if item.audio_reactive {
        badges = badges.push(badge("AUDIO", Color::from_rgb8(0x2f, 0x8c, 0xff)));
    }

    let mut facts: Vec<Element<'_, Message>> = Vec::new();
    let mut fact = |label: &'static str, value: String| {
        facts.push(
            row![
                text(label).size(13).color(theme::MUTED).width(90),
                text(value).size(13),
            ]
            .spacing(6)
            .into(),
        );
    };
    if item.size > 0 {
        fact("Size", human_size(item.size));
    }
    if let Some(t) = item.updated {
        fact("Updated", date(t));
    }
    if let Some(t) = item.installed {
        fact("Added", date(t));
    }
    if let Some(id) = &item.workshop_id {
        fact("Workshop", id.clone());
    }
    if !item.tags.is_empty() {
        fact("Tags", item.tags.join(", "));
    }

    let playing: Vec<&str> = app
        .status()
        .map(|s| {
            s.outputs
                .iter()
                .filter(|o| {
                    o.wallpaper
                        .as_ref()
                        .is_some_and(|w| std::path::Path::new(&w.path) == item.path)
                })
                .map(|o| o.name.as_str())
                .collect()
        })
        .unwrap_or_default();

    let applying = app.applying.as_deref() == Some(&item.id);
    let playable = !matches!(item.kind, Kind::Application);
    let apply_label = if applying {
        "Loading…".to_owned()
    } else {
        format!("Apply to {}", app.target.to_string().to_lowercase())
    };
    let apply = button(text(apply_label).size(14).center().width(Fill))
        .width(Fill)
        .padding([8, 12])
        .style(button::primary)
        .on_press_maybe(
            (playable && !applying && app.status().is_some()).then(|| Message::Apply(item.id.clone())),
        );

    let mut links = row![].spacing(8);
    if let Some(id) = &item.workshop_id {
        links = links.push(
            button(row![icons::external(13.0), text("Workshop page").size(13)].spacing(6).align_y(Alignment::Center))
                .style(button::secondary)
                .on_press(Message::OpenWorkshop(id.clone())),
        );
    }
    links = links.push(
        button(row![icons::folder(13.0), text("Open folder").size(13)].spacing(6).align_y(Alignment::Center))
            .style(button::secondary)
            .on_press(Message::OpenFolder(item.id.clone())),
    );

    let mut content = column![
        preview,
        text(&item.title).size(18),
        badges,
    ]
    .spacing(10);
    if !playing.is_empty() {
        content = content.push(
            text(format!("Showing on {}", playing.join(", ")))
                .size(13)
                .color(theme::SUCCESS),
        );
    }
    if matches!(item.kind, Kind::Application) {
        content = content.push(
            text("Application wallpapers aren't supported.")
                .size(13)
                .color(theme::WARNING),
        );
    }
    content = content
        .push(apply)
        .push(column(facts).spacing(4))
        .push(links);
    if let Some(form) = app.form.as_ref().filter(|f| f.item == item.id) {
        content = content.push(iced::widget::rule::horizontal(1)).push(form.view());
    }
    if let Some(description) = item.description.as_deref().filter(|d| !d.trim().is_empty()) {
        content = content.push(text(description).size(13).color(theme::MUTED));
    }

    container(scrollable(container(content).padding(14)).height(Fill))
        .width(DETAIL)
        .height(Fill)
        .style(theme::panel)
        .into()
}

fn human_size(bytes: u64) -> String {
    let mb = bytes as f64 / 1_048_576.0;
    if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else if mb >= 1.0 {
        format!("{mb:.0} MB")
    } else {
        format!("{} KB", bytes / 1024)
    }
}

/// `YYYY-MM-DD` of a Unix time (UTC).
fn date(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn dates() {
        assert_eq!(super::date(0), "1970-01-01");
        assert_eq!(super::date(951_782_400), "2000-02-29");
        assert_eq!(super::date(1_761_264_000), "2025-10-24");
    }
}
