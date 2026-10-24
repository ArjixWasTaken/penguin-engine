//! The app's state, messages and update.

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use iced::widget::image::Handle;
use iced::{Subscription, Task, Theme};
use wpe_ipc::{Event, Request, Status};
use wpe_library::locale::Locale;
use wpe_library::{Item, Kind, Rating, Source};

use crate::properties::{Edit, Form};
use crate::state::UiState;
use crate::{daemon, library, theme, thumbs, view};

pub struct App {
    pub items: Arc<Vec<Item>>,
    pub locale: Arc<Locale>,
    pub loading: bool,
    pub filters: Filters,
    pub sort: Sort,
    /// Indices into `items` that pass the filters, in order.
    pub visible: Vec<usize>,
    pub selected: Option<String>,
    pub hovered: Option<String>,
    pub thumbs: thumbs::Thumbs,
    /// The selected item's preview at full size.
    pub preview: Option<(String, Handle)>,
    pub daemon: DaemonState,
    pub target: Target,
    /// A wallpaper being set, by item id.
    pub applying: Option<String>,
    pub notice: Option<Notice>,
    /// The selected wallpaper's properties.
    pub form: Option<Form>,
    /// The selection to restore once the library is loaded.
    restore: Option<String>,
}

pub struct Filters {
    pub query: String,
    pub kinds: HashSet<Kind>,
    pub ratings: HashSet<Rating>,
    pub sources: HashSet<Source>,
    pub audio_only: bool,
}

impl Default for Filters {
    fn default() -> Self {
        Self {
            query: String::new(),
            kinds: [Kind::Scene, Kind::Video, Kind::Web, Kind::Preset].into(),
            ratings: [Rating::Everyone, Rating::Questionable, Rating::Mature, Rating::Unknown].into(),
            sources: [Source::Workshop, Source::Projects, Source::User].into(),
            audio_only: false,
        }
    }
}

impl Filters {
    fn admits(&self, item: &Item, words: &[String]) -> bool {
        let kind = match item.kind {
            Kind::Unknown => Kind::Scene,
            k => k,
        };
        self.kinds.contains(&kind)
            && self.ratings.contains(&item.rating)
            && self.sources.contains(&item.source)
            && (!self.audio_only || item.audio_reactive)
            && words.iter().all(|w| {
                item.title.to_lowercase().contains(w)
                    || item.tags.iter().any(|t| t.to_lowercase().contains(w))
                    || item.workshop_id.as_deref() == Some(w.as_str())
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sort {
    #[default]
    Title,
    Updated,
    Installed,
    Size,
}

impl Sort {
    pub const ALL: [Sort; 4] = [Sort::Title, Sort::Updated, Sort::Installed, Sort::Size];
}

impl fmt::Display for Sort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Sort::Title => "Name",
            Sort::Updated => "Last updated",
            Sort::Installed => "Recently added",
            Sort::Size => "Size",
        })
    }
}

/// Which outputs "Apply" changes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Target {
    #[default]
    All,
    Output(String),
}

impl Target {
    pub fn output(&self) -> Option<String> {
        match self {
            Target::All => None,
            Target::Output(name) => Some(name.clone()),
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Target::All => f.write_str("All displays"),
            Target::Output(name) => f.write_str(name),
        }
    }
}

#[derive(Default)]
pub enum DaemonState {
    #[default]
    Connecting,
    Connected(Status),
    Down(String),
}

pub struct Notice {
    pub text: String,
    pub error: bool,
}

#[derive(Debug, Clone)]
pub enum Message {
    Loaded(library::Loaded),
    Query(String),
    ToggleKind(Kind),
    ToggleRating(Rating),
    ToggleSource(Source),
    AudioOnly(bool),
    Sort(Sort),
    ClearFilters,
    TileShown(String),
    Thumb(String, Option<Handle>),
    Hover(String, bool),
    Select(String),
    Preview(String, Option<Handle>),
    Apply(String),
    Applied(String, Result<(), String>),
    Target(Target),
    Status(Result<Status, String>),
    RefreshStatus,
    Daemon(daemon::Feed),
    Pause(bool),
    Props(String, Option<String>, Result<library::Props, String>),
    Edit(Edit),
    PickFile(String, bool),
    OpenWorkshop(String),
    OpenFolder(String),
    DismissNotice,
    Ignore,
}

impl App {
    pub fn new() -> (Self, Task<Message>) {
        let saved = UiState::load();
        let app = Self {
            items: Arc::default(),
            locale: Arc::new(Locale::from_strings("en-us", Default::default())),
            loading: true,
            filters: saved.filters(),
            sort: saved.sort,
            restore: saved.selected,
            visible: Vec::new(),
            selected: None,
            hovered: None,
            thumbs: thumbs::Thumbs::default(),
            preview: None,
            daemon: DaemonState::default(),
            target: Target::default(),
            applying: None,
            notice: None,
            form: None,
        };
        let load = Task::perform(library::load(), Message::Loaded);
        let mut tasks = vec![load, status()];
        if let Some(path) = std::env::var_os("WPE_UI_SCREENSHOT") {
            tasks.push(screenshot(path.into()));
        }
        (app, Task::batch(tasks))
    }

    pub fn theme(&self) -> Theme {
        theme::theme()
    }

    pub fn title(&self) -> String {
        "Wallpapers".to_owned()
    }

    pub fn item(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    pub fn status(&self) -> Option<&Status> {
        match &self.daemon {
            DaemonState::Connected(status) => Some(status),
            _ => None,
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::run(daemon::events).map(Message::Daemon)
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Loaded(loaded) => {
                self.items = loaded.items;
                self.locale = loaded.locale;
                self.loading = false;
                self.refilter();
                // For screenshots: select the n-th visible item.
                if let Some(n) = std::env::var("WPE_UI_SELECT").ok().and_then(|n| n.parse::<usize>().ok())
                    && let Some(&i) = self.visible.get(n)
                {
                    return self.update(Message::Select(self.items[i].id.clone()));
                }
                if let Some(id) = self.restore.take().filter(|id| self.item(id).is_some()) {
                    return self.update(Message::Select(id));
                }
            }
            Message::Query(query) => {
                self.filters.query = query;
                self.refilter();
            }
            Message::ToggleKind(kind) => {
                toggle(&mut self.filters.kinds, kind);
                self.refilter();
            }
            Message::ToggleRating(rating) => {
                toggle(&mut self.filters.ratings, rating);
                self.refilter();
            }
            Message::ToggleSource(source) => {
                toggle(&mut self.filters.sources, source);
                self.refilter();
            }
            Message::AudioOnly(on) => {
                self.filters.audio_only = on;
                self.refilter();
            }
            Message::Sort(sort) => {
                self.sort = sort;
                self.refilter();
            }
            Message::ClearFilters => {
                self.filters = Filters::default();
                self.refilter();
            }
            Message::TileShown(id) => {
                if self.thumbs.shown(&id)
                    && let Some(preview) = self.item(&id).and_then(|i| i.preview.clone())
                {
                    return Task::perform(thumbs::load(preview), move |h| Message::Thumb(id.clone(), h));
                }
                if self.item(&id).is_some_and(|i| i.preview.is_none()) {
                    self.thumbs.loaded(id, None);
                }
            }
            Message::Thumb(id, handle) => self.thumbs.loaded(id, handle),
            Message::Hover(id, inside) => {
                if inside {
                    self.hovered = Some(id);
                } else if self.hovered.as_deref() == Some(&id) {
                    self.hovered = None;
                }
            }
            Message::Select(id) => {
                if self.selected.as_deref() == Some(&id) {
                    return Task::none();
                }
                self.selected = Some(id.clone());
                self.preview = None;
                self.form = None;
                self.save_state();
                let mut tasks = vec![self.load_form()];
                if let Some(preview) = self.item(&id).and_then(|i| i.preview.clone()) {
                    tasks.push(Task::perform(load_preview(preview), move |h| {
                        Message::Preview(id.clone(), h)
                    }));
                }
                return Task::batch(tasks);
            }
            Message::Preview(id, handle) => {
                if self.selected.as_deref() == Some(&id) {
                    self.preview = handle.map(|h| (id, h));
                }
            }
            Message::Apply(id) => {
                let Some(path) = self.item(&id).map(|i| i.path.to_string_lossy().into_owned()) else {
                    return Task::none();
                };
                self.applying = Some(id.clone());
                let request = Request::Set {
                    output: self.target.output(),
                    wallpaper: path,
                    properties: self
                        .form
                        .as_ref()
                        .filter(|f| f.item == id && !f.is_live())
                        .map(Form::changed)
                        .unwrap_or_default(),
                };
                return Task::perform(daemon::request(request), move |r| {
                    Message::Applied(id.clone(), r.map(|_| ()))
                });
            }
            Message::Applied(id, result) => {
                if self.applying.as_deref() == Some(&id) {
                    self.applying = None;
                }
                if let Err(e) = result {
                    let title = self.item(&id).map_or(id.clone(), |i| i.title.clone());
                    self.notice = Some(Notice {
                        text: format!("Couldn't set {title}: {e}"),
                        error: true,
                    });
                }
                return status();
            }
            Message::Target(target) => self.target = target,
            Message::Status(result) => {
                self.daemon = match result {
                    Ok(status) => {
                        if let Target::Output(name) = &self.target
                            && !status.outputs.iter().any(|o| &o.name == name)
                        {
                            self.target = Target::All;
                        }
                        DaemonState::Connected(status)
                    }
                    Err(e) => DaemonState::Down(e),
                };
                // The selected wallpaper started or stopped playing.
                let live = self.live_output();
                if self.selected.is_some()
                    && self.form.as_ref().map(|f| f.output.clone()) != Some(live)
                {
                    return self.load_form();
                }
            }
            Message::RefreshStatus => return status(),
            Message::Daemon(daemon::Feed::Down(e)) => {
                if !matches!(self.daemon, DaemonState::Down(_)) {
                    return self.update(Message::Status(Err(e)));
                }
            }
            Message::Daemon(daemon::Feed::Event(event)) => match event {
                Event::Status { status } => return self.update(Message::Status(Ok(status))),
                Event::Loading { .. } => {}
                Event::LoadFailed {
                    output,
                    wallpaper,
                    error,
                } => {
                    self.notice = Some(Notice {
                        text: format!("{output}: couldn't load {wallpaper}: {error}"),
                        error: true,
                    });
                }
                Event::Properties { output, values, .. } => {
                    if let Some(form) = &mut self.form
                        && form.output.as_deref() == Some(&output)
                    {
                        form.sync(values);
                    }
                }
            },
            Message::Pause(paused) => {
                let output = self.target.output();
                let request = if paused {
                    Request::Pause { output }
                } else {
                    Request::Resume { output }
                };
                return Task::perform(daemon::request(request), |_| Message::RefreshStatus);
            }
            Message::Props(id, output, result) => {
                if self.selected.as_deref() != Some(&id) {
                    return Task::none();
                }
                match result {
                    Ok((props, labels)) => self.form = Some(Form::new(id, output, props, labels)),
                    Err(e) => {
                        tracing::warn!("properties: {e}");
                        self.form = None;
                    }
                }
            }
            Message::Edit(edit) => {
                if let Some(form) = &mut self.form {
                    return form.update(edit);
                }
            }
            Message::PickFile(name, directory) => {
                return Task::perform(pick(directory), move |path| match path {
                    Some(path) => Message::Edit(Edit::Set(name.clone(), path.into())),
                    None => Message::Ignore,
                });
            }
            Message::OpenWorkshop(id) => open(&format!(
                "https://steamcommunity.com/sharedfiles/filedetails/?id={id}"
            )),
            Message::OpenFolder(id) => {
                if let Some(item) = self.item(&id) {
                    open(&item.path.to_string_lossy());
                }
            }
            Message::DismissNotice => self.notice = None,
            Message::Ignore => {}
        }
        Task::none()
    }

    pub fn view(&self) -> iced::Element<'_, Message> {
        view::view(self)
    }

    /// The output the selected wallpaper plays on, among the target ones.
    pub fn live_output(&self) -> Option<String> {
        let item = self.item(self.selected.as_deref()?)?;
        self.status()?
            .outputs
            .iter()
            .filter(|o| match &self.target {
                Target::All => true,
                Target::Output(name) => &o.name == name,
            })
            .find(|o| {
                o.wallpaper
                    .as_ref()
                    .is_some_and(|w| std::path::Path::new(&w.path) == item.path)
            })
            .map(|o| o.name.clone())
    }

    /// Fetch the selected wallpaper's properties: live ones from the daemon
    /// when it is shown, else those in its project.
    fn load_form(&mut self) -> Task<Message> {
        let Some(item) = self.selected.as_deref().and_then(|id| self.item(id)) else {
            return Task::none();
        };
        let (id, dir) = (item.id.clone(), item.path.clone());
        let output = self.live_output();
        let locale = self.locale.clone();
        let live = output.clone();
        let load = async move {
            let props = match live {
                Some(output) => {
                    let reply = daemon::request(Request::Properties {
                        output: Some(output.clone()),
                    })
                    .await?;
                    reply
                        .properties
                        .and_then(|list| list.into_iter().find(|p| p.output == output))
                        .map(|p| p.properties)
                        .ok_or_else(|| "no properties in the reply".to_owned())?
                }
                None => library::properties(dir.clone()).await,
            };
            let labels = library::labels(&locale, dir, &props);
            Ok((props, labels))
        };
        Task::perform(load, move |r| Message::Props(id.clone(), output.clone(), r))
    }

    fn save_state(&self) {
        UiState::of(&self.filters, self.sort, self.selected.clone()).save();
    }

    fn refilter(&mut self) {
        if !self.loading {
            self.save_state();
        }
        let words: Vec<String> = self
            .filters
            .query
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        let items = &self.items;
        self.visible = (0..items.len())
            .filter(|&i| self.filters.admits(&items[i], &words))
            .collect();
        match self.sort {
            Sort::Title => {}
            Sort::Updated => self.visible.sort_by_key(|&i| std::cmp::Reverse(items[i].updated)),
            Sort::Installed => self.visible.sort_by_key(|&i| std::cmp::Reverse(items[i].installed)),
            Sort::Size => self.visible.sort_by_key(|&i| std::cmp::Reverse(items[i].size)),
        }
    }
}

fn toggle<T: std::hash::Hash + Eq>(set: &mut HashSet<T>, value: T) {
    if !set.remove(&value) {
        set.insert(value);
    }
}

fn status() -> Task<Message> {
    Task::perform(daemon::request(Request::Status), |r| {
        Message::Status(r.and_then(|r| r.status.ok_or_else(|| "no status in the reply".to_owned())))
    })
}

async fn load_preview(path: std::path::PathBuf) -> Option<Handle> {
    tokio::task::spawn_blocking(move || {
        let image = image::open(&path).ok()?.into_rgba8();
        Some(Handle::from_rgba(image.width(), image.height(), image.into_raw()))
    })
    .await
    .ok()
    .flatten()
}

/// Save a screenshot of the window to `path` after a few seconds, then quit
/// (`WPE_UI_SCREENSHOT`, for checking the layout).
fn screenshot(path: std::path::PathBuf) -> Task<Message> {
    let delay = std::env::var("WPE_UI_SCREENSHOT_DELAY")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4.0);
    Task::future(tokio::time::sleep(std::time::Duration::from_secs_f64(delay)))
        .then(|_| iced::window::latest())
        .and_then(iced::window::screenshot)
        .then(move |shot| {
            let size = shot.size;
            match image::RgbaImage::from_raw(size.width, size.height, shot.rgba.to_vec()) {
                Some(img) => {
                    if let Err(e) = img.save(&path) {
                        tracing::error!("saving {}: {e}", path.display());
                    }
                }
                None => tracing::error!("bad screenshot"),
            }
            iced::exit()
        })
}

async fn pick(directory: bool) -> Option<String> {
    let dialog = rfd::AsyncFileDialog::new();
    let picked = if directory {
        dialog.pick_folder().await
    } else {
        dialog.pick_file().await
    };
    Some(picked?.path().to_string_lossy().into_owned())
}

fn open(target: &str) {
    if let Err(e) = std::process::Command::new("xdg-open").arg(target).spawn() {
        tracing::warn!("xdg-open {target}: {e}");
    }
}
