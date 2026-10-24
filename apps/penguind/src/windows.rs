//! Other applications' windows, for the rules (fullscreen, maximized,
//! focused, open), through KWin's `org_kde_plasma_window_management` and
//! its virtual desktops. Other compositors don't tell (window rules then
//! never hold).

use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols_plasma::plasma_virtual_desktop::client::{
    org_kde_plasma_virtual_desktop::{self, OrgKdePlasmaVirtualDesktop},
    org_kde_plasma_virtual_desktop_management::{self, OrgKdePlasmaVirtualDesktopManagement},
};
use wayland_protocols_plasma::plasma_window_management::client::{
    org_kde_plasma_window::{self, OrgKdePlasmaWindow},
    org_kde_plasma_window_management::{self, OrgKdePlasmaWindowManagement},
};
use wpe_ipc::WindowInfo;

use crate::App;

/// `org_kde_plasma_window_management.state` flags.
const ACTIVE: u32 = 0x1;
const MINIMIZED: u32 = 0x2;
const MAXIMIZED: u32 = 0x4;
const FULLSCREEN: u32 = 0x8;
const ON_ALL_DESKTOPS: u32 = 0x40;
const SKIP_TASKBAR: u32 = 0x1000;

/// App ids that are part of the desktop, or this project's app.
const NOT_APPLICATIONS: &[&str] = &[
    "org.kde.plasmashell",
    "org.kde.krunner",
    "org.kde.polkit-kde-authentication-agent-1",
    "penguin-engine",
    "com.github.arjixwastaken.penguin-engine",
];

struct Window {
    proxy: OrgKdePlasmaWindow,
    app: String,
    title: String,
    process: Option<String>,
    state: u32,
    /// x, y, width, height in global logical pixels.
    geometry: (i32, i32, u32, u32),
    desktops: Vec<String>,
    /// Its initial state came.
    ready: bool,
}

struct Desktop {
    proxy: OrgKdePlasmaVirtualDesktop,
    id: String,
    active: bool,
}

/// What the compositor says of the windows.
pub struct Windows {
    _manager: OrgKdePlasmaWindowManagement,
    _desktop_manager: Option<OrgKdePlasmaVirtualDesktopManagement>,
    windows: Vec<Window>,
    desktops: Vec<Desktop>,
    /// "Show desktop" hides every window.
    showing_desktop: bool,
}

/// An output's name and its rectangle (x, y, width, height) in global
/// logical pixels.
pub type OutputRect = (String, (i32, i32, i32, i32));

impl Windows {
    pub fn bind(globals: &GlobalList, qh: &QueueHandle<App>) -> Option<Self> {
        let manager: OrgKdePlasmaWindowManagement = globals.bind(qh, 13..=18, ()).ok()?;
        let desktop_manager = globals.bind(qh, 1..=2, ()).ok();
        Some(Self {
            _manager: manager,
            _desktop_manager: desktop_manager,
            windows: Vec::new(),
            desktops: Vec::new(),
            showing_desktop: false,
        })
    }

    fn window(&mut self, proxy: &OrgKdePlasmaWindow) -> Option<&mut Window> {
        self.windows.iter_mut().find(|w| w.proxy == *proxy)
    }

    /// Shown on the current virtual desktop (or it can't be told).
    fn on_current_desktop(&self, w: &Window) -> bool {
        w.state & ON_ALL_DESKTOPS != 0
            || w.desktops.is_empty()
            || !self.desktops.iter().any(|d| d.active)
            || self
                .desktops
                .iter()
                .any(|d| d.active && w.desktops.contains(&d.id))
    }

    /// Other applications' windows, each on the output its centre is on.
    pub fn infos(&self, outputs: &[OutputRect]) -> Vec<WindowInfo> {
        self.windows
            .iter()
            .filter(|w| w.ready && is_application(w))
            .map(|w| {
                let minimized = w.state & MINIMIZED != 0;
                let shown = !minimized && !self.showing_desktop && self.on_current_desktop(w);
                let (x, y, width, height) = w.geometry;
                let centre = (x + width as i32 / 2, y + height as i32 / 2);
                let output = shown
                    .then(|| {
                        outputs.iter().find(|(_, (ox, oy, ow, oh))| {
                            (*ox..ox + ow).contains(&centre.0) && (*oy..oy + oh).contains(&centre.1)
                        })
                    })
                    .flatten()
                    .map(|(name, _)| name.clone());
                WindowInfo {
                    app: w.app.clone(),
                    process: w.process.clone(),
                    title: w.title.clone(),
                    output,
                    focused: w.state & ACTIVE != 0 && !minimized,
                    maximized: w.state & MAXIMIZED != 0,
                    fullscreen: w.state & FULLSCREEN != 0,
                    minimized,
                }
            })
            .collect()
    }
}

fn is_application(w: &Window) -> bool {
    let named = |name: &str| {
        NOT_APPLICATIONS
            .iter()
            .any(|a| name.eq_ignore_ascii_case(a))
    };
    w.state & SKIP_TASKBAR == 0 && !named(&w.app) && !w.process.as_deref().is_some_and(named)
}

/// The name of process `pid`.
fn process_name(pid: u32) -> Option<String> {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(comm.trim_end().to_owned()).filter(|c| !c.is_empty())
}

impl Dispatch<OrgKdePlasmaWindowManagement, ()> for App {
    fn event(
        app: &mut Self,
        manager: &OrgKdePlasmaWindowManagement,
        event: org_kde_plasma_window_management::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let Some(windows) = &mut app.windows else {
            return;
        };
        match event {
            org_kde_plasma_window_management::Event::WindowWithUuid { uuid, .. } => {
                let proxy = manager.get_window_by_uuid(uuid, qh, ());
                windows.windows.push(Window {
                    proxy,
                    app: String::new(),
                    title: String::new(),
                    process: None,
                    state: 0,
                    geometry: (0, 0, 0, 0),
                    desktops: Vec::new(),
                    ready: false,
                });
            }
            org_kde_plasma_window_management::Event::ShowDesktopChanged { state } => {
                windows.showing_desktop = state != 0;
                app.signals_changed();
            }
            _ => {}
        }
    }
}

impl Dispatch<OrgKdePlasmaWindow, ()> for App {
    fn event(
        app: &mut Self,
        proxy: &OrgKdePlasmaWindow,
        event: org_kde_plasma_window::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use org_kde_plasma_window::Event;
        let Some(windows) = &mut app.windows else {
            return;
        };
        if let Event::Unmapped = event {
            windows.windows.retain(|w| w.proxy != *proxy);
            if proxy.version() >= 4 {
                proxy.destroy();
            }
            app.signals_changed();
            return;
        }
        let Some(w) = windows.window(proxy) else {
            return;
        };
        // Changes that matter to the rules.
        let matters = match event {
            Event::TitleChanged { title } => {
                w.title = title;
                false
            }
            Event::AppIdChanged { app_id } => {
                w.app = app_id;
                true
            }
            Event::PidChanged { pid } => {
                w.process = process_name(pid);
                true
            }
            Event::StateChanged { flags } => {
                let changed = (w.state ^ flags)
                    & (ACTIVE
                        | MINIMIZED
                        | MAXIMIZED
                        | FULLSCREEN
                        | ON_ALL_DESKTOPS
                        | SKIP_TASKBAR)
                    != 0;
                w.state = flags;
                changed
            }
            Event::Geometry {
                x,
                y,
                width,
                height,
            } => {
                w.geometry = (x, y, width, height);
                true
            }
            Event::VirtualDesktopEntered { id } => {
                w.desktops.push(id);
                true
            }
            Event::VirtualDesktopLeft { is } => {
                w.desktops.retain(|d| *d != is);
                true
            }
            Event::InitialState => {
                w.ready = true;
                true
            }
            _ => false,
        };
        if matters && w.ready {
            app.signals_changed();
        }
    }
}

impl Dispatch<OrgKdePlasmaVirtualDesktopManagement, ()> for App {
    fn event(
        app: &mut Self,
        manager: &OrgKdePlasmaVirtualDesktopManagement,
        event: org_kde_plasma_virtual_desktop_management::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let Some(windows) = &mut app.windows else {
            return;
        };
        match event {
            org_kde_plasma_virtual_desktop_management::Event::DesktopCreated {
                desktop_id, ..
            } => {
                let proxy = manager.get_virtual_desktop(desktop_id.clone(), qh, ());
                windows.desktops.push(Desktop {
                    proxy,
                    id: desktop_id,
                    active: false,
                });
            }
            org_kde_plasma_virtual_desktop_management::Event::DesktopRemoved { desktop_id } => {
                windows.desktops.retain(|d| d.id != desktop_id);
                app.signals_changed();
            }
            _ => {}
        }
    }
}

impl Dispatch<OrgKdePlasmaVirtualDesktop, ()> for App {
    fn event(
        app: &mut Self,
        proxy: &OrgKdePlasmaVirtualDesktop,
        event: org_kde_plasma_virtual_desktop::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(windows) = &mut app.windows else {
            return;
        };
        let Some(desktop) = windows.desktops.iter_mut().find(|d| d.proxy == *proxy) else {
            return;
        };
        match event {
            org_kde_plasma_virtual_desktop::Event::Activated => desktop.active = true,
            org_kde_plasma_virtual_desktop::Event::Deactivated => desktop.active = false,
            _ => return,
        }
        app.signals_changed();
    }
}
