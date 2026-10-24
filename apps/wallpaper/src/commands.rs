//! Requests from the control socket, and the settings changes they make.

use std::path::Path;
use std::time::Instant;

use serde_json::Value;
use smithay_client_toolkit::reexports::client::Proxy;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::SurfaceKind;
use wpe_ipc::{
    Fill, OutputProperties, OutputStatus, PropertyChoice, PropertyInfo, Request, Response,
    Settings, Status, WallpaperInfo,
};
use wpe_runtime::{Project, Wallpaper};

use crate::loading::{LoadSpec, SetRequest};
use crate::state::SavedOutput;
use crate::{App, control, shell_layer, state};

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
/// Wallpaper Engine shows them; `current` gives a property's value now.
pub fn property_infos(
    project: &Project,
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
            default: p.value.clone(),
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

fn wallpaper_info(path: &Path, wallpaper: &Wallpaper) -> WallpaperInfo {
    let project = wallpaper.project();
    let kind = match &project.kind {
        Some(kind) => kind.to_ascii_lowercase(),
        None if wallpaper.is_video() => "video".to_owned(),
        None => "scene".to_owned(),
    };
    WallpaperInfo {
        path: path.display().to_string(),
        workshop_id: project
            .workshopid
            .clone()
            .filter(|id| !id.is_empty())
            .or_else(|| wpe_runtime::steam::workshop_id(path)),
        title: wallpaper.title.clone(),
        kind,
    }
}

fn check_settings(settings: &Settings) -> Result<(), String> {
    if settings.volume > 100 {
        return Err(format!("volume {} is out of 0..=100", settings.volume));
    }
    if settings.fps > MAX_FPS {
        return Err(format!("fps {} is above {MAX_FPS}", settings.fps));
    }
    Ok(())
}

impl control::Handler for App {
    fn handle_request(&mut self, request: Request, responder: control::Responder) {
        let result = match request {
            Request::Status => Ok(Response {
                status: Some(self.status()),
                ..Response::ok()
            }),
            // Answered once the wallpaper has loaded.
            Request::Set { output, wallpaper } => {
                if let Err(e) = self.set_wallpaper(output.as_deref(), &wallpaper, responder) {
                    tracing::warn!("control request failed: {e}");
                }
                return;
            }
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
            Request::Fps { fps } => self.change_settings(|s| s.fps = fps),
            Request::Fill { output, fill } => self.set_fill(output.as_deref(), fill),
            Request::ReloadSettings => self.reload_settings().map(|()| Response::ok()),
            Request::Quit => {
                tracing::info!("asked to quit");
                self.exit = true;
                Ok(Response::ok())
            }
        };
        if let Err(e) = &result {
            tracing::warn!("control request failed: {e}");
        }
        responder.send(Response::from_result(result));
    }
}

impl App {
    fn status(&self) -> Status {
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
                OutputStatus {
                    name: o.name.clone(),
                    width: logical.0,
                    height: logical.1,
                    scale,
                    wallpaper: screen.map(|s| wallpaper_info(&s.path, &s.wallpaper)),
                    paused: screen.is_some_and(|s| s.paused),
                    fps: screen
                        .filter(|s| !s.paused)
                        .map_or(0.0, |s| s.stats.current()),
                    fill,
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
                properties: Vec::new(),
                fill,
                request: Some(request.clone()),
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
                    properties: property_infos(s.wallpaper.project(), |name| {
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
        }
        self.save_state();
        self.schedule_all();
        Ok(Response::ok())
    }

    fn pause(&mut self, output: Option<&str>, paused: bool) -> Result<Response, String> {
        for i in self.select_screens(output)? {
            self.screens[i].set_paused(paused);
        }
        self.update_status();
        self.schedule_all();
        Ok(Response::ok())
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
            for s in &mut self.screens {
                s.wallpaper.set_volume(new.volume as f32 / 100.0);
            }
        }
        if old.audio != new.audio {
            self.apply_audio();
        }
        if old.fps != new.fps {
            let now = Instant::now();
            for s in &mut self.screens {
                s.next_frame = now;
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
        let list = property_infos(&project, |name| (name == "speed").then(|| json!(2.5)));
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
