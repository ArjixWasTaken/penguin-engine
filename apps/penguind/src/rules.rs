//! The rules ([`wpe_ipc::Rule`]): what the session is doing (windows,
//! sound, battery, idle time) decides each output's [`Limits`], which pause,
//! mute, stop, slow down or thin out its wallpaper.

use std::time::{Duration, Instant};

use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::client::protocol::wl_output::WlOutput;
use wpe_ipc::{Condition, Limits, Playback, Rule, Signals, WallpaperInfo, WindowInfo, WindowState};

use crate::App;

/// Signals settle this long before the rules are checked again (a window
/// changes state several times in a row).
const SETTLE: Duration = Duration::from_millis(200);
/// A stop holds this long before the wallpaper is unloaded (paused until
/// then), so a passing window doesn't cost a reload.
const STOP_AFTER: Duration = Duration::from_secs(3);

/// An output whose wallpaper a rule stopped.
pub struct Stopped {
    pub output: WlOutput,
    pub name: String,
    /// What it showed.
    pub info: WallpaperInfo,
    pub limits: Limits,
}

/// What the enabled `rules` holding make of the wallpaper on `output`.
pub fn limits_for(rules: &[Rule], signals: &Signals, output: &str) -> Limits {
    let mut limits = Limits::default();
    for rule in rules.iter().filter(|r| r.enabled) {
        if let Some(why) = holds(rule, signals, output) {
            limits.add(rule, why);
        }
    }
    limits
}

/// Why `rule` holds for `output`, if it does.
fn holds(rule: &Rule, signals: &Signals, output: &str) -> Option<String> {
    match &rule.when {
        Condition::Window { state, app } => {
            let windows = signals.windows.as_deref()?;
            let here = |w: &WindowInfo| {
                w.output.as_deref() == Some(output) || (rule.all_outputs && w.output.is_some())
            };
            let w = windows.iter().find(|w| {
                let names = [Some(w.app.as_str()), w.process.as_deref()];
                let app_matches = app
                    .as_deref()
                    .is_none_or(|wanted| names.iter().flatten().any(|n| is_app(wanted, n)));
                app_matches
                    && match state {
                        WindowState::Open => true,
                        WindowState::Focused => w.focused && here(w),
                        WindowState::Maximized => (w.maximized || w.fullscreen) && here(w),
                        WindowState::Fullscreen => w.fullscreen && here(w),
                    }
            })?;
            let what = match state {
                WindowState::Open => "is running",
                WindowState::Focused => "is focused",
                WindowState::Maximized if w.fullscreen => "is fullscreen",
                WindowState::Maximized => "is maximized",
                WindowState::Fullscreen => "is fullscreen",
            };
            Some(format!("{} {what}", window_name(w)))
        }
        Condition::Audio { app } => {
            let playing = signals.audio.as_deref()?;
            let name = playing
                .iter()
                .find(|n| app.as_deref().is_none_or(|wanted| is_app(wanted, n)))?;
            Some(match name.is_empty() {
                true => "an application plays sound".to_owned(),
                false => format!("{name} plays sound"),
            })
        }
        Condition::Battery { below } => {
            let battery = signals.battery.filter(|b| b.discharging)?;
            match below {
                None => Some("on battery".to_owned()),
                Some(below) => battery
                    .percent
                    .filter(|p| p < below)
                    .map(|_| format!("battery below {below}%")),
            }
        }
        Condition::Idle { seconds } => (signals.idle? >= *seconds && *seconds > 0)
            .then(|| format!("no input for {}", duration(*seconds))),
    }
}

/// `name` (an app id, process or sound name) is the application `wanted`:
/// the same, case aside, or its last dotted part (`org.mozilla.firefox`
/// is `firefox`).
fn is_app(wanted: &str, name: &str) -> bool {
    let wanted = wanted.trim();
    let name = name.strip_suffix(".desktop").unwrap_or(name);
    !wanted.is_empty()
        && (name.eq_ignore_ascii_case(wanted)
            || name
                .rsplit('.')
                .next()
                .is_some_and(|last| last.eq_ignore_ascii_case(wanted)))
}

fn window_name(w: &WindowInfo) -> &str {
    [
        Some(w.app.as_str()),
        w.process.as_deref(),
        Some(w.title.as_str()),
    ]
    .into_iter()
    .flatten()
    .find(|n| !n.is_empty())
    .unwrap_or("a window")
}

fn duration(seconds: u32) -> String {
    match seconds {
        s if s % 3600 == 0 => format!("{} h", s / 3600),
        s if s % 60 == 0 => format!("{} min", s / 60),
        s => format!("{s} s"),
    }
}

/// The idle times the enabled rules wait for.
pub fn idle_times(rules: &[Rule]) -> Vec<u32> {
    let mut times: Vec<u32> = rules
        .iter()
        .filter(|r| r.enabled)
        .filter_map(|r| match r.when {
            Condition::Idle { seconds } if seconds > 0 => Some(seconds),
            _ => None,
        })
        .collect();
    times.sort_unstable();
    times.dedup();
    times
}

impl App {
    /// What the rules are checked against.
    pub fn signals(&self) -> Signals {
        let outputs: Vec<_> = self
            .outputs
            .iter()
            .filter_map(|o| {
                let info = self.output_state.info(&o.output)?;
                let (x, y) = info.logical_position?;
                let (w, h) = info.logical_size?;
                Some((o.name.clone(), (x, y, w, h)))
            })
            .collect();
        Signals {
            windows: self.windows.as_ref().map(|w| w.infos(&outputs)),
            audio: self.sound.clone(),
            battery: self.battery,
            idle: self.idle.as_ref().map(|i| i.idle_for()),
        }
    }

    /// Something the rules look at changed: check them once it settles.
    pub fn signals_changed(&mut self) {
        if self.rules_pending {
            return;
        }
        self.rules_pending = true;
        let armed = self
            .loop_handle
            .insert_source(Timer::from_duration(SETTLE), |_, _, app| {
                app.rules_pending = false;
                app.apply_rules();
                TimeoutAction::Drop
            });
        if let Err(e) = armed {
            tracing::warn!("cannot arm a timer: {e}");
            self.rules_pending = false;
        }
    }

    /// Watch for the idle times the rules use.
    pub fn watch_idle(&mut self) {
        let times = idle_times(&self.settings.rules);
        let seat = self.seat_state.seats().next();
        if let Some(idle) = &mut self.idle {
            idle.watch(&times, seat.as_ref(), &self.qh);
        }
    }

    /// Check the rules and hold each wallpaper back as they say.
    pub fn apply_rules(&mut self) {
        let signals = self.signals();
        let rules = self.settings.rules.clone();
        let now = Instant::now();
        let mut changed = false;
        let mut i = 0;
        while i < self.screens.len() {
            let limits = limits_for(&rules, &signals, &self.screens[i].name);
            let screen = &mut self.screens[i];
            if limits.playback == Playback::Stop {
                let since = *screen.stop_since.get_or_insert(now);
                if now >= since + STOP_AFTER {
                    let screen = self.screens.remove(i);
                    tracing::info!(
                        "{}: stopping \"{}\" ({})",
                        screen.name,
                        screen.wallpaper.title,
                        limits.because.join(", ")
                    );
                    let volume = self.state.volume(&screen.name, &screen.path);
                    self.stopped.push(Stopped {
                        output: screen.output.clone(),
                        name: screen.name.clone(),
                        info: crate::commands::wallpaper_info(&screen, volume),
                        limits,
                    });
                    drop(screen);
                    changed = true;
                    continue;
                }
                self.recheck_at(since + STOP_AFTER);
            } else {
                self.screens[i].stop_since = None;
            }
            let screen = &mut self.screens[i];
            if !screen.held.same_effect(&limits) {
                tracing::info!(
                    "{}: {:?}, fps {:?}, particles {:?} ({})",
                    screen.name,
                    limits.playback,
                    limits.fps,
                    limits.particles,
                    limits.because.join(", ")
                );
            }
            changed |= screen.held != limits;
            screen.held = limits;
            self.apply_limits(i);
            i += 1;
        }
        // Stopped wallpapers come back once no stop holds.
        let mut j = 0;
        while j < self.stopped.len() {
            let limits = limits_for(&rules, &signals, &self.stopped[j].name);
            if limits.playback == Playback::Stop {
                changed |= self.stopped[j].limits != limits;
                self.stopped[j].limits = limits;
                j += 1;
                continue;
            }
            let stopped = self.stopped.remove(j);
            changed = true;
            let busy = self.screen_for(&stopped.output).is_some()
                || self.loads.loading(&stopped.output).is_some();
            if busy {
                continue;
            }
            if let Some(saved) = self.state.for_output(&stopped.name).cloned() {
                tracing::info!("{}: restarting \"{}\"", stopped.name, stopped.info.title);
                if let Err(e) = self.create_screen(&stopped.output, &stopped.name, &saved) {
                    tracing::error!("{}: {e:#}", stopped.name);
                }
            }
        }
        if changed {
            self.apply_audio();
            self.apply_volumes();
            self.update_status();
        }
        self.schedule_all();
    }

    /// Check the rules again at `at`.
    fn recheck_at(&mut self, at: Instant) {
        let armed = self
            .loop_handle
            .insert_source(Timer::from_deadline(at), |_, _, app| {
                app.signals_changed();
                TimeoutAction::Drop
            });
        if let Err(e) = armed {
            tracing::warn!("cannot arm a timer: {e}");
        }
    }

    /// Put screen `i`'s limits (and the global particle setting) in effect.
    pub fn apply_limits(&mut self, i: usize) {
        let particles = self.settings.particles.min(100) as f32 / 100.0;
        let screen = &mut self.screens[i];
        let own = screen
            .held
            .particles
            .map_or(1.0, |p| p.min(100) as f32 / 100.0);
        screen.wallpaper.set_particle_density(particles * own);
        screen.apply_pause();
        let volume = self.state.volume(&screen.name, &screen.path);
        let global = self.settings.volume as f32 / 100.0;
        let screen = &mut self.screens[i];
        screen.apply_volume(global * volume as f32 / 100.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wpe_ipc::Battery;

    fn window(app: &str, output: Option<&str>) -> WindowInfo {
        WindowInfo {
            app: app.into(),
            title: "Title".into(),
            output: output.map(Into::into),
            ..Default::default()
        }
    }

    fn rule(when: Condition, playback: Playback) -> Rule {
        Rule::new(when, playback)
    }

    #[test]
    fn window_rules_follow_the_output() {
        let mut game = window("steam_app_1", Some("DP-1"));
        game.fullscreen = true;
        let mut editor = window("org.kde.kate", Some("DP-2"));
        editor.maximized = true;
        editor.focused = true;
        let signals = Signals {
            windows: Some(vec![game, editor]),
            ..Default::default()
        };
        let rules = Rule::defaults();
        assert_eq!(
            limits_for(&rules, &signals, "DP-1").playback,
            Playback::Pause
        );
        assert_eq!(
            limits_for(&rules, &signals, "DP-1").because,
            ["steam_app_1 is fullscreen"]
        );
        assert!(limits_for(&rules, &signals, "DP-2").is_none());

        let maximized = Condition::Window {
            state: WindowState::Maximized,
            app: None,
        };
        let rules = vec![rule(maximized.clone(), Playback::Mute)];
        // A fullscreen window is maximized too.
        assert_eq!(
            limits_for(&rules, &signals, "DP-1").playback,
            Playback::Mute
        );
        assert_eq!(
            limits_for(&rules, &signals, "DP-2").playback,
            Playback::Mute
        );
        assert!(limits_for(&rules, &signals, "HDMI-A-1").is_none());
        let mut everywhere = rule(maximized, Playback::Mute);
        everywhere.all_outputs = true;
        assert_eq!(
            limits_for(&[everywhere], &signals, "HDMI-A-1").playback,
            Playback::Mute
        );

        // By app: the last part of an app id, any case.
        let kate = vec![rule(
            Condition::Window {
                state: WindowState::Focused,
                app: Some("Kate".into()),
            },
            Playback::Stop,
        )];
        assert_eq!(limits_for(&kate, &signals, "DP-2").playback, Playback::Stop);
        assert!(limits_for(&kate, &signals, "DP-1").is_none());
    }

    #[test]
    fn hidden_windows_only_count_as_running() {
        let mut hidden = window("firefox", None);
        hidden.fullscreen = true;
        hidden.minimized = true;
        let signals = Signals {
            windows: Some(vec![hidden]),
            ..Default::default()
        };
        assert!(limits_for(&Rule::defaults(), &signals, "DP-1").is_none());
        let running = vec![rule(
            Condition::Window {
                state: WindowState::Open,
                app: Some("firefox".into()),
            },
            Playback::Pause,
        )];
        assert_eq!(
            limits_for(&running, &signals, "DP-1").because,
            ["firefox is running"]
        );
    }

    #[test]
    fn sound_battery_and_idle() {
        let mut slow = rule(Condition::Battery { below: None }, Playback::Run);
        slow.fps = Some(30);
        let mut low = rule(Condition::Battery { below: Some(20) }, Playback::Stop);
        low.particles = Some(10);
        let rules = vec![
            rule(Condition::Audio { app: None }, Playback::Mute),
            rule(
                Condition::Audio {
                    app: Some("spotify".into()),
                },
                Playback::Pause,
            ),
            slow,
            low,
            rule(Condition::Idle { seconds: 300 }, Playback::Pause),
        ];
        let mut signals = Signals {
            audio: Some(vec!["firefox".into()]),
            battery: Some(Battery {
                discharging: true,
                percent: Some(50),
            }),
            idle: Some(0),
            ..Default::default()
        };
        let limits = limits_for(&rules, &signals, "DP-1");
        assert_eq!(limits.playback, Playback::Mute);
        assert_eq!(limits.fps, Some(30));
        assert_eq!(limits.because, ["firefox plays sound", "on battery"]);

        signals.battery = Some(Battery {
            discharging: true,
            percent: Some(15),
        });
        signals.idle = Some(300);
        let limits = limits_for(&rules, &signals, "DP-1");
        assert_eq!(limits.playback, Playback::Stop);
        assert_eq!(limits.particles, Some(10));
        assert!(limits.because.contains(&"no input for 5 min".to_owned()));

        // Plugged in, nothing playing, input: nothing holds.
        let signals = Signals {
            audio: Some(Vec::new()),
            battery: Some(Battery {
                discharging: false,
                percent: Some(15),
            }),
            idle: Some(0),
            ..Default::default()
        };
        assert!(limits_for(&rules, &signals, "DP-1").is_none());
        assert_eq!(idle_times(&rules), [300]);
    }

    #[test]
    fn disabled_rules_and_unknown_signals_never_hold() {
        let mut off = Rule::defaults().remove(0);
        off.enabled = false;
        let mut game = window("game", Some("DP-1"));
        game.fullscreen = true;
        let signals = Signals {
            windows: Some(vec![game]),
            ..Default::default()
        };
        assert!(limits_for(&[off], &signals, "DP-1").is_none());
        let rules = vec![
            rule(Condition::Audio { app: None }, Playback::Mute),
            rule(Condition::Battery { below: None }, Playback::Pause),
            rule(Condition::Idle { seconds: 60 }, Playback::Pause),
        ];
        assert!(limits_for(&rules, &Signals::default(), "DP-1").is_none());
    }
}
