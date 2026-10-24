//! Requests from the control socket, and the settings changes they make.

use std::collections::BTreeMap;
use std::time::Instant;

use serde_json::Value;
use smithay_client_toolkit::reexports::client::Proxy;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::SurfaceKind;
use wpe_ipc::{
    Event, Fill, OutputProperties, OutputStatus, PropertyChoice, PropertyInfo, Request, Response,
    Settings, SettingsPatch, Status, Values, VersionInfo, WallpaperInfo,
};
use wpe_runtime::Project;

use crate::loading::{LoadSpec, SetRequest};
use crate::state::SavedOutput;
use crate::{App, Screen, control, shell_layer, state};

/// Fastest frame rate cap accepted.
const MAX_FPS: u32 = 1000;

/// Indices of the outputs a request means: the one named `wanted`, or all.
pub fn select<'a>(
    names: impl IntoIterator<Item = &'a str>,
    wanted: Option<&str>,
) -> Result<Vec<usize>, String> {
    let names: Vec<&str> = names.into_iter().collect();
    match wanted {
        Some(wanted) => names
            .iter()
            .position(|n| *n == wanted)
            .map(|i| vec![i])
            .ok_or_else(|| {
                if names.is_empty() {
                    format!("no output {wanted}: no outputs")
                } else {
                    format!("no output {wanted} (outputs: {})", names.join(", "))
                }
            }),
        None if names.is_empty() => Err("no outputs".to_owned()),
        None => Ok((0..names.len()).collect()),
    }
}

/// The user properties of `project` for a settings form, in the order
/// Wallpaper Engine shows them; `defaults` are the values without overrides
/// (else `project`'s), `current` gives a property's value now.
pub fn property_infos(
    project: &Project,
    defaults: &BTreeMap<String, Value>,
    current: impl Fn(&str) -> Option<Value>,
) -> Vec<PropertyInfo> {
    let mut list: Vec<PropertyInfo> = project
        .general
        .properties
        .iter()
        .map(|(name, p)| PropertyInfo {
            name: name.clone(),
            kind: p.kind.clone().unwrap_or_default(),
            text: p.text.clone(),
            value: current(name).unwrap_or_else(|| p.value.clone()),
            default: defaults.get(name).unwrap_or(&p.value).clone(),
            options: p
                .options
                .iter()
                .map(|o| PropertyChoice {
                    label: o.label.clone(),
                    value: o.value.clone(),
                })
                .collect(),
            min: p.min,
            max: p.max,
            step: p.step,
            order: p.order,
            condition: p.condition.clone(),
        })
        .collect();
    list.sort_by(|a, b| {
        let order = |p: &PropertyInfo| p.order.unwrap_or(f64::MAX);
        order(a)
            .total_cmp(&order(b))
            .then_with(|| a.name.cmp(&b.name))
    });
    list
}

/// What `screen` shows. For a preset item: the item's path, id and title,
/// the dependency's type, and the dependency.
/// What `screen` shows; it plays at `volume` percent of the global volume.
pub fn wallpaper_info(screen: &Screen, volume: u32) -> WallpaperInfo {
    let (path, wallpaper) = (&screen.path, &screen.wallpaper);
    let project = wallpaper.project();
    let kind = match &project.kind {
        Some(kind) => kind.to_ascii_lowercase(),
        None if wallpaper.is_video() => "video".to_owned(),
        None => "scene".to_owned(),
    };
    let own_id = || {
        project
            .workshopid
            .clone()
            .filter(|id| !id.is_empty())
            .or_else(|| wpe_runtime::steam::workshop_id(path))
    };
    match &screen.preset {
        Some(preset) => WallpaperInfo {
            path: path.display().to_string(),
            workshop_id: preset
                .workshop_id
                .clone()
                .or_else(|| wpe_runtime::steam::workshop_id(path)),
            title: preset
                .title
                .clone()
                .unwrap_or_else(|| wallpaper.title.clone()),
            kind,
            dependency: Some(preset.dependency.display().to_string()),
            sound: wallpaper.has_sound(),
            volume,
        },
        None => WallpaperInfo {
            path: path.display().to_string(),
            workshop_id: own_id(),
            title: wallpaper.title.clone(),
            kind,
            dependency: None,
            sound: wallpaper.has_sound(),
            volume,
        },
    }
}

/// The current values of every user property of `screen`'s wallpaper.
fn current_values(screen: &Screen) -> Values {
    screen
        .wallpaper
        .project()
        .general
        .properties
        .iter()
        .map(|(name, p)| {
            let value = screen.wallpaper.user_property(name).unwrap_or(&p.value);
            (name.clone(), value.clone())
        })
        .collect()
}

/// What the daemon is and supports.
pub fn version() -> VersionInfo {
    let mut features = Vec::new();
    if cfg!(feature = "web") {
        features.push("web".to_owned());
    }
    VersionInfo {
        protocol: wpe_ipc::PROTOCOL,
        daemon: env!("CARGO_PKG_VERSION").to_owned(),
        features,
    }
}

fn check_settings(settings: &Settings) -> Result<(), String> {
    if settings.volume > 100 {
        return Err(format!("volume {} is out of 0..=100", settings.volume));
    }
    if settings.fps > MAX_FPS {
        return Err(format!("fps {} is above {MAX_FPS}", settings.fps));
    }
    if settings.particles > 100 {
        return Err(format!("particles {}% is above 100%", settings.particles));
    }
    for rule in &settings.rules {
        if rule.fps.is_some_and(|f| f == 0 || f > MAX_FPS) {
            return Err(format!("a rule's fps must be within 1..={MAX_FPS}"));
        }
        if rule.particles.is_some_and(|p| p > 100) {
            return Err("a rule's particles must be within 0..=100%".to_owned());
        }
    }
    Ok(())
}

impl control::Handler for App {
    fn handle_request(&mut self, request: Request, responder: control::Responder) {
        // Changes to properties don't show in the status.
        let changes_status = !matches!(
            request,
            Request::Status
                | Request::Properties { .. }
                | Request::Property { .. }
                | Request::Version
                | Request::Signals
        );
        let result = match request {
            Request::Status => Ok(Response {
                status: Some(self.status()),
                ..Response::ok()
            }),
            // Answered once the wallpaper has loaded.
            Request::Set {
                output,
                wallpaper,
                properties,
            } => {
                let properties = properties.into_iter().collect();
                if let Err(e) =
                    self.set_wallpaper(output.as_deref(), &wallpaper, properties, responder)
                {
                    tracing::warn!("control request failed: {e}");
                }
                return;
            }
            Request::Version => Ok(Response {
                version: Some(version()),
                ..Response::ok()
            }),
            Request::Signals => Ok(Response {
                signals: Some(self.signals()),
                ..Response::ok()
            }),
            Request::Subscribe => {
                if let Some(stream) = responder.into_subscriber() {
                    let first = Event::Status {
                        status: self.status(),
                    };
                    if let Err(e) = self.events.add(stream, Some(&first)) {
                        tracing::warn!("cannot subscribe a client: {e}");
                    }
                }
                return;
            }
            // Answered at once, or once the outputs that reload have.
            Request::SetProperties { output, values } => {
                let values = values.into_iter().collect();
                self.change_properties(output.as_deref(), Change::Set(values), responder);
                return;
            }
            Request::ResetProperties { output } => {
                self.change_properties(output.as_deref(), Change::Reset, responder);
                return;
            }
            Request::Settings { patch } => self.patch_settings(&patch),
            Request::Properties { output } => self.properties(output.as_deref()),
            Request::Property {
                output,
                name,
                value,
            } => self.set_property(output.as_deref(), &name, value),
            Request::Pause { output } => self.pause(output.as_deref(), true),
            Request::Resume { output } => self.pause(output.as_deref(), false),
            Request::Audio { enabled } => self.change_settings(|s| s.audio = enabled),
            Request::Volume { volume } => self.change_settings(|s| s.volume = volume),
            Request::WallpaperVolume { output, volume } => {
                self.set_wallpaper_volume(output.as_deref(), volume)
            }
            Request::Fps { fps } => self.change_settings(|s| s.fps = fps),
            Request::Fill { output, fill } => self.set_fill(output.as_deref(), fill),
            Request::ReloadSettings => self.reload_settings().map(|()| Response::ok()),
            Request::Quit => {
                tracing::info!("asked to quit");
                self.exit = true;
                Ok(Response::ok())
            }
        };
        if changes_status {
            self.status_changed();
        }
        if let Err(e) = &result {
            tracing::warn!("control request failed: {e}");
        }
        responder.send(Response::from_result(result));
    }
}

impl App {
    pub fn status(&self) -> Status {
        let outputs = self
            .outputs
            .iter()
            .map(|o| {
                let screen = self.screen_for(&o.output).map(|i| &self.screens[i]);
                let info = self.output_state.info(&o.output);
                let logical = screen
                    .map(|s| s.logical)
                    .filter(|&(w, h)| w > 0 && h > 0)
                    .or_else(|| {
                        let (w, h) = info.as_ref()?.logical_size?;
                        Some((w.max(0) as u32, h.max(0) as u32))
                    })
                    .unwrap_or_default();
                let scale = match screen {
                    Some(s) if s.logical.0 > 0 => s.config.width as f32 / s.logical.0 as f32,
                    _ => info.as_ref().map_or(1.0, |i| i.scale_factor as f32),
                };
                let fill = screen.map(|s| s.fill).unwrap_or_else(|| {
                    self.state
                        .outputs
                        .get(&o.name)
                        .and_then(|s| s.fill)
                        .unwrap_or(self.settings.fill)
                });
                let stopped = self.stopped.iter().find(|s| s.output == o.output);
                OutputStatus {
                    name: o.name.clone(),
                    width: logical.0,
                    height: logical.1,
                    scale,
                    wallpaper: screen
                        .map(|s| wallpaper_info(s, self.state.volume(&s.name, &s.path)))
                        .or_else(|| stopped.map(|s| s.info.clone())),
                    paused: screen.is_some_and(|s| s.frozen()) || stopped.is_some(),
                    fps: screen
                        .filter(|s| !s.frozen())
                        .map_or(0.0, |s| s.stats.current()),
                    limits: screen
                        .map(|s| s.held.clone())
                        .or_else(|| stopped.map(|s| s.limits.clone()))
                        .unwrap_or_default(),
                    fill,
                    loading: self
                        .loads
                        .loading(&o.output)
                        .map(|p| p.display().to_string()),
                }
            })
            .collect();
        Status {
            settings: self.settings.clone(),
            outputs,
        }
    }

    /// Indices of the screens (outputs showing a wallpaper) a request means.
    fn select_screens(&self, wanted: Option<&str>) -> Result<Vec<usize>, String> {
        if let Some(wanted) = wanted
            && self.outputs.iter().any(|o| o.name == wanted)
            && !self.screens.iter().any(|s| s.name == wanted)
        {
            return Err(format!("output {wanted} shows no wallpaper"));
        }
        if wanted.is_none() && self.screens.is_empty() {
            return Err("no output shows a wallpaper".to_owned());
        }
        select(self.screens.iter().map(|s| s.name.as_str()), wanted)
    }

    /// Start loading `spec` for the outputs `output` means; `responder`
    /// answers once every one has loaded (and shows it), or one failed (and
    /// none changes). Errors found before loading are answered at once and
    /// returned.
    fn set_wallpaper(
        &mut self,
        output: Option<&str>,
        spec: &str,
        properties: BTreeMap<String, Value>,
        responder: control::Responder,
    ) -> Result<(), String> {
        let checked = (|| {
            let path = wpe_runtime::steam::resolve_wallpaper(spec)?;
            let path = path.canonicalize().unwrap_or(path);
            let targets = select(self.outputs.iter().map(|o| o.name.as_str()), output)?;
            Ok::<_, String>((path, targets))
        })();
        let (path, targets) = match checked {
            Ok(v) => v,
            Err(e) => {
                responder.send(Response::error(e.clone()));
                return Err(e);
            }
        };
        // Outputs without a wallpaper (nor one loading) get a surface first.
        let mut surfaces = Vec::new();
        for &t in &targets {
            let (output, name) = (self.outputs[t].output.clone(), self.outputs[t].name.clone());
            if self.screen_for(&output).is_some() || self.loads.has_surface_for(&output) {
                surfaces.push(None);
                continue;
            }
            match self.new_surface(&output, &name) {
                Ok(surface) => surfaces.push(Some(surface)),
                Err(e) => {
                    let e = format!("{name}: {e:#}");
                    responder.send(Response::error(e.clone()));
                    return Err(e);
                }
            }
        }
        tracing::info!("loading {}", path.display());
        let request = SetRequest::new(responder);
        for (&t, surface) in targets.iter().zip(surfaces) {
            let (output, name) = (self.outputs[t].output.clone(), self.outputs[t].name.clone());
            let fill = self
                .state
                .outputs
                .get(&name)
                .and_then(|o| o.fill)
                .unwrap_or(self.settings.fill);
            let spec = LoadSpec {
                output,
                name,
                path: path.clone(),
                properties: properties.clone(),
                fill,
                request: Some(request.clone()),
                remember: true,
            };
            self.start_load(spec, surface);
        }
        Ok(())
    }

    fn properties(&self, output: Option<&str>) -> Result<Response, String> {
        let list = self
            .select_screens(output)?
            .into_iter()
            .map(|i| {
                let s = &self.screens[i];
                OutputProperties {
                    output: s.name.clone(),
                    wallpaper: s.path.display().to_string(),
                    properties: property_infos(s.wallpaper.project(), &s.defaults, |name| {
                        s.wallpaper.user_property(name).cloned()
                    }),
                }
            })
            .collect();
        Ok(Response {
            properties: Some(list),
            ..Response::ok()
        })
    }

    fn set_property(
        &mut self,
        output: Option<&str>,
        name: &str,
        value: Value,
    ) -> Result<Response, String> {
        let has = |app: &App, i: usize| {
            app.screens[i]
                .wallpaper
                .project()
                .general
                .properties
                .contains_key(name)
        };
        let mut targets = self.select_screens(output)?;
        if output.is_some() {
            if let Some(&i) = targets.first()
                && !has(self, i)
            {
                return Err(format!(
                    "\"{}\" has no property {name}",
                    self.screens[i].wallpaper.title
                ));
            }
        } else {
            targets.retain(|&i| has(self, i));
            if targets.is_empty() {
                return Err(format!("no wallpaper has a property {name}"));
            }
        }
        let gpu = self.gpu.clone().ok_or("no GPU")?;
        for i in targets {
            let screen = &mut self.screens[i];
            let started = Instant::now();
            screen
                .wallpaper
                .set_user_property(&gpu, name, value.clone());
            screen.dirty = true;
            tracing::info!(
                "{}: property {name} = {value} ({:.1?})",
                screen.name,
                started.elapsed()
            );
            self.state
                .set_property(&screen.name, &screen.path, name, value.clone());
            self.emit_properties(i);
        }
        self.save_state();
        self.schedule_all();
        Ok(Response::ok())
    }

    /// Tell subscribers screen `i`'s property values.
    pub fn emit_properties(&self, i: usize) {
        if self.events.is_empty() {
            return;
        }
        let screen = &self.screens[i];
        self.events.send(&Event::Properties {
            output: screen.name.clone(),
            wallpaper: screen.path.display().to_string(),
            values: current_values(screen),
        });
    }

    /// `set-properties` and `reset-properties`. A change of one value (or
    /// any on a web wallpaper, whose page takes them as they come) applies
    /// in place; several on a scene reload it with all of them off the
    /// render thread, one rebuild instead of one each, and the response
    /// waits for that.
    fn change_properties(
        &mut self,
        output: Option<&str>,
        change: Change,
        responder: control::Responder,
    ) {
        let targets = match self.property_targets(output, &change) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("control request failed: {e}");
                responder.send(Response::error(e));
                return;
            }
        };
        let Some(gpu) = self.gpu.clone() else {
            responder.send(Response::error("no GPU"));
            return;
        };
        let mut reloads = Vec::new();
        for (i, changes) in targets {
            let screen = &self.screens[i];
            let (name, path) = (screen.name.clone(), screen.path.clone());
            match &change {
                Change::Set(values) => {
                    let project = &screen.wallpaper.project().general.properties;
                    for (k, v) in values.iter().filter(|(k, _)| project.contains_key(*k)) {
                        self.state.set_property(&name, &path, k, v.clone());
                    }
                }
                Change::Reset => self.state.clear_properties(&name, &path),
            }
            if changes.is_empty() {
                continue;
            }
            let screen = &mut self.screens[i];
            if changes.len() == 1 || screen.wallpaper.is_web() {
                let started = Instant::now();
                for (k, v) in &changes {
                    screen.wallpaper.set_user_property(&gpu, k, v.clone());
                }
                screen.dirty = true;
                tracing::info!(
                    "{name}: {} properties changed ({:.1?})",
                    changes.len(),
                    started.elapsed()
                );
                self.emit_properties(i);
            } else {
                tracing::info!("{name}: {} properties changed; reloading", changes.len());
                reloads.push(i);
            }
        }
        self.save_state();
        if reloads.is_empty() {
            self.schedule_all();
            responder.send(Response::ok());
            return;
        }
        let request = SetRequest::new(responder);
        for i in reloads {
            let screen = &self.screens[i];
            let spec = LoadSpec {
                output: screen.output.clone(),
                name: screen.name.clone(),
                path: screen.path.clone(),
                properties: self.state.overrides(&screen.name, &screen.path),
                fill: screen.fill,
                request: Some(request.clone()),
                remember: false,
            };
            self.start_load(spec, None);
        }
        self.schedule_all();
    }

    /// The screens `change` applies to, each with the values that differ
    /// from what it shows now.
    fn property_targets(
        &self,
        output: Option<&str>,
        change: &Change,
    ) -> Result<Vec<(usize, Changes)>, String> {
        let screens = self.select_screens(output)?;
        let mut known = false;
        let mut targets = Vec::new();
        for i in screens {
            let screen = &self.screens[i];
            let project = &screen.wallpaper.project().general.properties;
            let wanted: Vec<(String, Value)> = match change {
                Change::Set(values) => values
                    .iter()
                    .filter(|(k, _)| project.contains_key(*k))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                Change::Reset => project
                    .iter()
                    .map(|(k, p)| {
                        (
                            k.clone(),
                            screen.defaults.get(k).unwrap_or(&p.value).clone(),
                        )
                    })
                    .collect(),
            };
            known |= !wanted.is_empty();
            let changes = wanted
                .into_iter()
                .filter(|(k, v)| screen.wallpaper.user_property(k) != Some(v))
                .collect();
            targets.push((i, changes));
        }
        if let Change::Set(values) = change
            && !values.is_empty()
            && !known
        {
            let names: Vec<&str> = values.keys().map(String::as_str).collect();
            return Err(format!(
                "no wallpaper has the properties {}",
                names.join(", ")
            ));
        }
        Ok(targets)
    }

    fn pause(&mut self, output: Option<&str>, paused: bool) -> Result<Response, String> {
        for i in self.select_screens(output)? {
            self.screens[i].set_paused(paused);
        }
        self.update_status();
        self.schedule_all();
        Ok(Response::ok())
    }

    fn set_wallpaper_volume(
        &mut self,
        output: Option<&str>,
        volume: u32,
    ) -> Result<Response, String> {
        if volume > 100 {
            return Err(format!("volume {volume} is out of range (0-100)"));
        }
        for i in self.select_screens(output)? {
            let screen = &self.screens[i];
            self.state.set_volume(&screen.name, &screen.path, volume);
        }
        self.apply_volumes();
        self.save_state();
        Ok(Response::ok())
    }

    /// Play each screen's wallpaper at its own volume times the global one.
    pub fn apply_volumes(&mut self) {
        for i in 0..self.screens.len() {
            self.apply_limits(i);
        }
    }

    fn set_fill(&mut self, output: Option<&str>, fill: Fill) -> Result<Response, String> {
        match output {
            Some(_) => {
                for i in self.select_screens(output)? {
                    let screen = &mut self.screens[i];
                    screen.set_fill(fill);
                    self.state.set_fill(&screen.name, &screen.path, Some(fill));
                }
                self.save_state();
                self.schedule_all();
                Ok(Response::ok())
            }
            // The global fill, for every output.
            None => {
                for saved in self.state.outputs.values_mut() {
                    saved.fill = None;
                }
                self.change_settings(|s| s.fill = fill)
            }
        }
    }

    /// `settings`: several at once, as the single-setting requests do them.
    fn patch_settings(&mut self, patch: &SettingsPatch) -> Result<Response, String> {
        let new = self.settings.patched(patch);
        check_settings(&new)?;
        if patch.fill.is_some() {
            // The global fill, for every output.
            for saved in self.state.outputs.values_mut() {
                saved.fill = None;
            }
        }
        self.change_settings(|s| *s = new)
    }

    /// Change settings over the socket: applied now and saved, ahead of the
    /// config file until it is reloaded.
    fn change_settings(&mut self, change: impl FnOnce(&mut Settings)) -> Result<Response, String> {
        let mut settings = self.settings.clone();
        change(&mut settings);
        check_settings(&settings)?;
        self.apply_settings(settings);
        self.state.settings = Some(self.settings.clone());
        self.save_state();
        Ok(Response::ok())
    }

    /// Re-read the config file (SIGHUP, `reload-settings`): it and the
    /// command line decide the settings again.
    pub fn reload_settings(&mut self) -> Result<(), String> {
        let config = match &self.config_path {
            Some(path) => state::load_config(path).map_err(|e| format!("{e:#}"))?,
            None => Settings::default(),
        };
        let settings = self.cli.apply(config);
        check_settings(&settings)?;
        self.apply_settings(settings);
        self.state.settings = None;
        self.save_state();
        tracing::info!("settings reloaded: {:?}", self.settings);
        Ok(())
    }

    /// Make `new` the settings in effect, changing what differs live.
    fn apply_settings(&mut self, new: Settings) {
        let old = std::mem::replace(&mut self.settings, new.clone());
        if old.layer != new.layer {
            let can_move = self.screens.iter().all(|s| match s.layer.kind() {
                SurfaceKind::Wlr(wlr) => wlr.version() >= 2,
                _ => false,
            });
            if can_move {
                for s in &self.screens {
                    s.layer.set_layer(shell_layer(new.layer));
                    s.layer.commit();
                }
            } else {
                self.recreate_screens();
            }
        }
        if old.passthrough != new.passthrough {
            for s in &self.screens {
                s.set_passthrough(&self.compositor, new.passthrough);
                s.layer.commit();
            }
        }
        if old.fill != new.fill {
            for s in &mut self.screens {
                let own = self.state.outputs.get(&s.name).and_then(|o| o.fill);
                if own.is_none() {
                    s.set_fill(new.fill);
                }
            }
        }
        if old.volume != new.volume {
            self.apply_volumes();
        }
        if old.audio != new.audio {
            self.apply_audio();
        }
        if old.rules != new.rules {
            self.watch_idle();
        }
        if old.rules != new.rules || old.particles != new.particles {
            self.apply_rules();
        }
        if old.fps != new.fps {
            let now = Instant::now();
            let general = crate::general_settings(&new);
            for s in &mut self.screens {
                s.next_frame = now;
                s.wallpaper.apply_general_settings(&general);
            }
        }
        self.schedule_all();
    }

    /// Rebuild every screen (for a layer change the compositor can't make
    /// on an existing surface).
    fn recreate_screens(&mut self) {
        let screens: Vec<_> = self.screens.drain(..).collect();
        for s in screens {
            let mut saved = SavedOutput::new(s.path.clone());
            if let Some(own) = self.state.outputs.get(&s.name)
                && own.wallpaper == s.path
            {
                saved = own.clone();
            }
            let (output, name) = (s.output.clone(), s.name.clone());
            drop(s);
            if let Err(e) = self.create_screen(&output, &name, &saved) {
                tracing::error!("{name}: {e:#}");
            }
        }
    }
}

/// Property values to change, in order.
type Changes = Vec<(String, Value)>;

/// What `set-properties` and `reset-properties` do.
enum Change {
    Set(BTreeMap<String, Value>),
    /// Back to the defaults.
    Reset,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn selects_outputs() {
        let names = ["DP-1", "HDMI-A-1"];
        assert_eq!(select(names, None).unwrap(), vec![0, 1]);
        assert_eq!(select(names, Some("HDMI-A-1")).unwrap(), vec![1]);
        let err = select(names, Some("DP-2")).unwrap_err();
        assert!(err.contains("DP-1, HDMI-A-1"), "{err}");
        assert!(select([], None).is_err());
        assert!(select([], Some("DP-1")).is_err());
    }

    #[test]
    fn lists_properties_in_order() {
        let project: Project = serde_json::from_value(json!({
            "title": "Test",
            "type": "scene",
            "general": {"properties": {
                "schemecolor": {"order": 0, "text": "ui_browse_properties_scheme_color",
                                "type": "color", "value": "0 0 0"},
                "speed": {"order": 2, "text": "Speed", "type": "slider",
                          "value": 1, "min": 0, "max": 5, "step": 0.1},
                "mode": {"order": 1, "text": "Mode", "type": "combo", "value": "a",
                         "options": [{"label": "A", "value": "a"}, {"label": "B", "value": "b"}]},
                "rain": {"text": "Rain", "type": "bool", "value": true}
            }}
        }))
        .unwrap();
        let defaults = BTreeMap::from([("rain".to_owned(), json!(false))]);
        let list = property_infos(&project, &defaults, |name| {
            (name == "speed").then(|| json!(2.5))
        });
        let names: Vec<_> = list.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["schemecolor", "mode", "speed", "rain"]);
        let speed = &list[2];
        assert_eq!(speed.kind, "slider");
        assert_eq!(speed.value, json!(2.5));
        assert_eq!(speed.default, json!(1));
        assert_eq!(
            (speed.min, speed.max, speed.step),
            (Some(0.0), Some(5.0), Some(0.1))
        );
        assert_eq!(list[1].options.len(), 2);
        assert_eq!(list[1].options[1].value, json!("b"));
        assert_eq!(list[3].value, json!(true));
        assert_eq!(list[3].default, json!(false));
    }

    #[test]
    fn settings_limits() {
        assert!(check_settings(&Settings::default()).is_ok());
        let loud = Settings {
            volume: 101,
            ..Default::default()
        };
        assert!(check_settings(&loud).is_err());
        let fast = Settings {
            fps: MAX_FPS + 1,
            ..Default::default()
        };
        assert!(check_settings(&fast).is_err());
    }
}
