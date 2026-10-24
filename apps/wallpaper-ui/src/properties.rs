//! The selected wallpaper's user properties as a form. Changes to a
//! wallpaper on screen apply live; at most one request per property is in
//! flight, and the newest value follows when it returns, so slider drags
//! go as fast as the daemon rebuilds and no faster.

use std::collections::{HashMap, HashSet};

use iced::widget::{button, column, pick_list, row, slider, text, text_input, toggler};
use iced::{Alignment, Color, Element, Fill, Task};
use serde_json::Value;
use wpe_ipc::{PropertyInfo, Request};
use wpe_library::presets::{self, Preset};

use crate::app::Message;
use crate::{color_picker, daemon, icons, theme};

pub struct Form {
    /// The library item these are for.
    pub item: String,
    /// The output it plays on, where changes go; None when it isn't shown,
    /// and values wait for Apply.
    pub output: Option<String>,
    pub props: Vec<PropertyInfo>,
    labels: HashMap<String, String>,
    pub values: HashMap<String, Value>,
    inflight: HashSet<String>,
    queued: HashMap<String, Value>,
    /// Colour fields being typed into.
    drafts: HashMap<String, String>,
    /// The colour property whose picker is open.
    picking: Option<String>,
    presets: Vec<Preset>,
    preset_name: String,
}

#[derive(Debug, Clone)]
pub enum Edit {
    Set(String, Value),
    Draft(String, String),
    Pick(String),
    PresetName(String),
    SavePreset,
    LoadPreset(String),
    DeletePreset(String),
    Sent(String, Result<(), String>),
    Reset,
}

impl Form {
    pub fn new(
        item: String,
        output: Option<String>,
        props: Vec<PropertyInfo>,
        labels: HashMap<String, String>,
    ) -> Self {
        let values = props
            .iter()
            .map(|p| (p.name.clone(), p.value.clone()))
            .collect();
        Self {
            output,
            props,
            labels,
            values,
            inflight: HashSet::new(),
            queued: HashMap::new(),
            drafts: HashMap::new(),
            picking: None,
            presets: presets::default_dir()
                .map(|dir| presets::list(&dir, &item))
                .unwrap_or_default(),
            preset_name: String::new(),
            item,
        }
    }

    pub fn is_live(&self) -> bool {
        self.output.is_some()
    }

    /// Values that differ from the defaults (sent with Apply).
    pub fn changed(&self) -> serde_json::Map<String, Value> {
        self.props
            .iter()
            .filter_map(|p| {
                let v = self.values.get(&p.name)?;
                (*v != p.default).then(|| (p.name.clone(), v.clone()))
            })
            .collect()
    }

    /// The daemon's values now; those still being sent stay as typed.
    pub fn sync(&mut self, values: serde_json::Map<String, Value>) {
        for (name, value) in values {
            if !self.inflight.contains(&name) && !self.drafts.contains_key(&name) {
                self.values.insert(name, value);
            }
        }
    }

    pub fn update(&mut self, edit: Edit) -> Task<Message> {
        match edit {
            Edit::Set(name, value) => {
                self.drafts.remove(&name);
                self.values.insert(name.clone(), value.clone());
                if self.inflight.contains(&name) {
                    self.queued.insert(name, value);
                    return Task::none();
                }
                self.send(name, value)
            }
            Edit::Draft(name, draft) => {
                if let Some(color) = parse_hex(&draft) {
                    self.drafts.insert(name.clone(), draft);
                    let value = Value::String(color_value(color));
                    self.values.insert(name.clone(), value.clone());
                    if self.inflight.contains(&name) {
                        self.queued.insert(name, value);
                        return Task::none();
                    }
                    return self.send(name, value);
                }
                self.drafts.insert(name, draft);
                Task::none()
            }
            Edit::PresetName(name) => {
                self.preset_name = name;
                Task::none()
            }
            Edit::SavePreset => {
                let name = self.preset_name.trim().to_owned();
                let Some(dir) = presets::default_dir().filter(|_| !name.is_empty()) else {
                    return Task::none();
                };
                let preset = Preset {
                    name,
                    wallpaper: self.item.clone(),
                    values: self.changed(),
                };
                if let Err(e) = presets::save(&dir, &preset) {
                    tracing::warn!("saving preset: {e}");
                    return Task::none();
                }
                self.presets.retain(|p| p.name != preset.name);
                self.presets.push(preset);
                self.presets.sort_by_cached_key(|p| p.name.to_lowercase());
                self.preset_name.clear();
                Task::none()
            }
            Edit::LoadPreset(name) => {
                let Some(preset) = self.presets.iter().find(|p| p.name == name) else {
                    return Task::none();
                };
                // Unset properties go back to their defaults.
                let mut values = serde_json::Map::new();
                for p in &self.props {
                    let value = preset.values.get(&p.name).unwrap_or(&p.default).clone();
                    self.values.insert(p.name.clone(), value.clone());
                    values.insert(p.name.clone(), value);
                }
                self.drafts.clear();
                self.queued.clear();
                let Some(output) = self.output.clone() else {
                    return Task::none();
                };
                Task::perform(
                    daemon::request(Request::SetProperties {
                        output: Some(output),
                        values,
                    }),
                    |r| Message::Edit(Edit::Sent(String::new(), r.map(|_| ()))),
                )
            }
            Edit::DeletePreset(name) => {
                if let Some(dir) = presets::default_dir()
                    && let Err(e) = presets::delete(&dir, &self.item, &name)
                {
                    tracing::warn!("deleting preset: {e}");
                    return Task::none();
                }
                self.presets.retain(|p| p.name != name);
                Task::none()
            }
            Edit::Pick(name) => {
                self.picking = if self.picking.as_deref() == Some(&name) {
                    None
                } else {
                    Some(name)
                };
                Task::none()
            }
            Edit::Sent(name, result) => {
                self.inflight.remove(&name);
                if let Err(e) = result {
                    tracing::warn!("setting {name}: {e}");
                }
                match self.queued.remove(&name) {
                    Some(value) => self.send(name, value),
                    None => Task::none(),
                }
            }
            Edit::Reset => {
                self.drafts.clear();
                self.queued.clear();
                for p in &self.props {
                    self.values.insert(p.name.clone(), p.default.clone());
                }
                let Some(output) = self.output.clone() else {
                    return Task::none();
                };
                Task::perform(
                    daemon::request(Request::ResetProperties {
                        output: Some(output),
                    }),
                    |r| Message::Edit(Edit::Sent(String::new(), r.map(|_| ()))),
                )
            }
        }
    }

    fn send(&mut self, name: String, value: Value) -> Task<Message> {
        let Some(output) = self.output.clone() else {
            return Task::none();
        };
        self.inflight.insert(name.clone());
        let request = Request::Property {
            output: Some(output),
            name: name.clone(),
            value,
        };
        Task::perform(daemon::request(request), move |r| {
            Message::Edit(Edit::Sent(name.clone(), r.map(|_| ())))
        })
    }

    pub fn view(&self) -> Element<'_, Message> {
        let visible: HashMap<String, Value> = self.values.clone();
        let mut rows = column![].spacing(10);
        for p in &self.props {
            if let Some(condition) = &p.condition
                && !wpe_library::condition::visible(condition, &visible)
            {
                continue;
            }
            if let Some(row) = self.field(p) {
                rows = rows.push(row);
            }
        }
        let reset = button(
            row![icons::reset(12.0), text("Reset").size(13)]
                .spacing(6)
                .align_y(Alignment::Center),
        )
        .style(button::secondary)
        .on_press(Message::Edit(Edit::Reset));
        let mut content = column![
            row![
                text("PROPERTIES").size(11).color(theme::MUTED).width(Fill),
                reset
            ]
            .align_y(Alignment::Center),
        ]
        .spacing(10);
        if !self.is_live() {
            content = content.push(
                text("Changes apply when you apply the wallpaper.")
                    .size(12)
                    .color(theme::MUTED),
            );
        }
        content.push(self.presets_view()).push(rows).into()
    }

    fn presets_view(&self) -> Element<'_, Message> {
        let names: Vec<String> = self.presets.iter().map(|p| p.name.clone()).collect();
        let mut column = column![].spacing(6);
        if !names.is_empty() {
            column = column.push(
                pick_list(names, None::<String>, |name| {
                    Message::Edit(Edit::LoadPreset(name))
                })
                .placeholder("Load a preset…")
                .text_size(13)
                .width(Fill),
            );
        }
        let save = button(text("Save").size(13))
            .style(button::secondary)
            .on_press_maybe(
                (!self.preset_name.trim().is_empty()).then_some(Message::Edit(Edit::SavePreset)),
            );
        let mut line = row![
            text_input("New preset name", &self.preset_name)
                .on_input(|s| Message::Edit(Edit::PresetName(s)))
                .on_submit(Message::Edit(Edit::SavePreset))
                .size(13),
            save,
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        if let Some(existing) = self
            .presets
            .iter()
            .find(|p| p.name == self.preset_name.trim())
        {
            line = line.push(
                button(text("Delete").size(13))
                    .style(button::danger)
                    .on_press(Message::Edit(Edit::DeletePreset(existing.name.clone()))),
            );
        }
        column.push(line).into()
    }

    fn field<'a>(&'a self, p: &'a PropertyInfo) -> Option<Element<'a, Message>> {
        let name = p.name.clone();
        let value = self.values.get(&p.name).unwrap_or(&p.value);
        let label = self
            .labels
            .get(&p.name)
            .cloned()
            .unwrap_or_else(|| p.name.clone());
        let kind = p.kind.to_ascii_lowercase();
        let row: Element<'a, Message> = match kind.as_str() {
            "bool" => toggler(truthy(value))
                .label(label)
                .text_size(13)
                .size(18)
                .on_toggle(move |on| Message::Edit(Edit::Set(name.clone(), Value::Bool(on))))
                .into(),
            "slider" => {
                let min = p.min.unwrap_or(0.0);
                let max = p.max.unwrap_or(1.0).max(min + f64::EPSILON);
                let step = p.step.filter(|s| *s > 0.0).unwrap_or((max - min) / 100.0);
                let current = number(value).unwrap_or(min).clamp(min, max);
                let integer = step.fract() == 0.0 && min.fract() == 0.0;
                let shown = if integer {
                    format!("{current:.0}")
                } else {
                    format!("{current:.2}")
                };
                column![
                    row![
                        text(label).size(13).width(Fill),
                        text(shown).size(13).color(theme::MUTED)
                    ],
                    slider(min..=max, current, move |v| {
                        let v = if integer {
                            Value::from(v.round() as i64)
                        } else {
                            Value::from(v)
                        };
                        Message::Edit(Edit::Set(name.clone(), v))
                    })
                    .step(step),
                ]
                .spacing(4)
                .into()
            }
            "combo" => {
                let options: Vec<Choice> = p
                    .options
                    .iter()
                    .map(|o| Choice {
                        label: o
                            .label
                            .as_deref()
                            .map(wpe_library::locale::plain)
                            .unwrap_or_else(|| o.value.to_string()),
                        value: o.value.clone(),
                    })
                    .collect();
                let selected = options
                    .iter()
                    .find(|o| loosely_equal(&o.value, value))
                    .cloned();
                column![
                    text(label).size(13),
                    pick_list(options, selected, move |c: Choice| {
                        Message::Edit(Edit::Set(name.clone(), c.value))
                    })
                    .text_size(13)
                    .width(Fill),
                ]
                .spacing(4)
                .into()
            }
            "color" => {
                let color = parse_color(value).unwrap_or(Color::WHITE);
                let draft = self
                    .drafts
                    .get(&p.name)
                    .cloned()
                    .unwrap_or_else(|| hex(color));
                let swatch = button(text(""))
                    .width(28)
                    .height(28)
                    .on_press(Message::Edit(Edit::Pick(p.name.clone())))
                    .style(move |_, _| button::Style {
                        background: Some(color.into()),
                        border: iced::border::rounded(theme::RADIUS)
                            .color(theme::MUTED)
                            .width(1),
                        ..Default::default()
                    });
                let line = row![
                    text(label).size(13).width(Fill),
                    swatch,
                    text_input("#rrggbb", &draft)
                        .on_input(move |s| Message::Edit(Edit::Draft(name.clone(), s)))
                        .size(13)
                        .width(90),
                ]
                .spacing(8)
                .align_y(Alignment::Center);
                if self.picking.as_deref() == Some(&p.name) {
                    let name = p.name.clone();
                    column![
                        line,
                        color_picker::picker(color, move |c| {
                            Message::Edit(Edit::Set(name.clone(), Value::String(color_value(c))))
                        }),
                    ]
                    .spacing(6)
                    .into()
                } else {
                    line.into()
                }
            }
            "textinput" => {
                let current = value
                    .as_str()
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| value.to_string());
                column![
                    text(label).size(13),
                    text_input("", &current)
                        .on_input(move |s| Message::Edit(Edit::Set(name.clone(), Value::String(s))))
                        .size(13),
                ]
                .spacing(4)
                .into()
            }
            "text" => text(label).size(13).color(theme::MUTED).into(),
            "group" => text(label.to_uppercase())
                .size(11)
                .color(theme::MUTED)
                .into(),
            "file" | "scenetexture" | "directory" => {
                let current = value.as_str().filter(|s| !s.is_empty()).unwrap_or("None");
                column![
                    text(label).size(13),
                    row![
                        text(current).size(12).color(theme::MUTED).width(Fill),
                        button(text("Choose…").size(12))
                            .style(button::secondary)
                            .on_press(Message::PickFile(p.name.clone(), kind == "directory")),
                        button(text("Clear").size(12))
                            .style(button::text)
                            .on_press(Message::Edit(Edit::Set(
                                p.name.clone(),
                                Value::String(String::new())
                            ))),
                    ]
                    .spacing(6)
                    .align_y(Alignment::Center),
                ]
                .spacing(4)
                .into()
            }
            _ => return None,
        };
        Some(row)
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Choice {
    label: String,
    value: Value,
}

impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        Value::String(s) => s == "1" || s.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        _ => None,
    }
}

fn loosely_equal(a: &Value, b: &Value) -> bool {
    a == b
        || match (number(a), number(b)) {
            (Some(x), Some(y)) => x == y,
            _ => a.as_str().is_some() && a.as_str() == b.as_str(),
        }
}

/// `"r g b"` with components 0..1 (some wallpapers use 0..255).
fn parse_color(v: &Value) -> Option<Color> {
    let parts: Vec<f32> = v
        .as_str()?
        .split_whitespace()
        .filter_map(|s| s.parse().ok())
        .collect();
    let [r, g, b] = parts.get(..3)?.try_into().ok()?;
    let scale = if r > 1.0 || g > 1.0 || b > 1.0 {
        255.0
    } else {
        1.0
    };
    Some(Color::from_rgb(r / scale, g / scale, b / scale))
}

fn color_value(c: Color) -> String {
    let f = |x: f32| {
        let s = format!("{:.5}", x);
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    };
    format!("{} {} {}", f(c.r), f(c.g), f(c.b))
}

fn hex(c: Color) -> String {
    let [r, g, b, _] = c.into_rgba8();
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn parse_hex(s: &str) -> Option<Color> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(s, 16).ok()?;
    Some(Color::from_rgb8((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn colors() {
        let c = parse_color(&json!("1 0.5 0")).unwrap();
        assert_eq!(hex(c), "#ff8000");
        assert_eq!(hex(parse_color(&json!("255 128 0")).unwrap()), "#ff8000");
        assert_eq!(color_value(parse_hex("#ff0000").unwrap()), "1 0 0");
    }

    #[test]
    fn loose_values() {
        assert!(loosely_equal(&json!(1), &json!("1")));
        assert!(loosely_equal(&json!("a"), &json!("a")));
        assert!(!loosely_equal(&json!("a"), &json!("b")));
    }
}
