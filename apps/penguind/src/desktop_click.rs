//! Right clicks belong to the desktop, as in Wallpaper Engine: its menu
//! opens through the wallpaper. Wayland routes a click to the surface under
//! the pointer whatever the button, so a right press hides our input region
//! and, on release, the click is played again with KWin's fake input
//! (`org_kde_kwin_fake_input`), reaching what is underneath; then the
//! region comes back. Without that protocol, right clicks stay with the
//! wallpaper.

use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, QueueHandle};
use wayland_protocols_plasma::fake_input::client::org_kde_kwin_fake_input::OrgKdeKwinFakeInput;

use crate::{App, BTN_RIGHT};

/// `wl_pointer.button_state`.
const PRESSED: u32 = 1;
const RELEASED: u32 = 0;

pub struct DesktopClicks {
    fake: OrgKdeKwinFakeInput,
}

impl DesktopClicks {
    pub fn bind(globals: &GlobalList, qh: &QueueHandle<App>) -> Option<Self> {
        let fake: OrgKdeKwinFakeInput = globals.bind(qh, 1..=4, ()).ok()?;
        fake.authenticate(
            "Penguin Engine".to_owned(),
            "Pass right clicks on the wallpaper to the desktop".to_owned(),
        );
        Some(Self { fake })
    }

    /// Click the right button on whatever is under the pointer now.
    pub fn right_click(&self) {
        // A nudge there and back, so the compositor looks again at what is
        // under the pointer (our surface no longer takes input).
        self.fake.pointer_motion(1.0, 0.0);
        self.fake.pointer_motion(-1.0, 0.0);
        self.fake.button(BTN_RIGHT, PRESSED);
        self.fake.button(BTN_RIGHT, RELEASED);
    }
}

impl Dispatch<OrgKdeKwinFakeInput, ()> for App {
    fn event(
        _: &mut Self,
        _: &OrgKdeKwinFakeInput,
        _: <OrgKdeKwinFakeInput as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
