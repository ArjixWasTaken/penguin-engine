//! Time without input, for idle rules: one `ext_idle_notification_v1` per
//! idle time the rules use. Apps that inhibit idling (a video playing)
//! keep the session from counting as idle.

use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, QueueHandle};
use smithay_client_toolkit::reexports::protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1::{self, ExtIdleNotificationV1},
    ext_idle_notifier_v1::ExtIdleNotifierV1,
};

use crate::App;

struct Notification {
    proxy: ExtIdleNotificationV1,
    seconds: u32,
    idle: bool,
}

pub struct Idle {
    notifier: ExtIdleNotifierV1,
    notifications: Vec<Notification>,
}

impl Idle {
    pub fn bind(globals: &GlobalList, qh: &QueueHandle<App>) -> Option<Self> {
        let notifier = globals.bind(qh, 1..=1, ()).ok()?;
        Some(Self {
            notifier,
            notifications: Vec::new(),
        })
    }

    /// Be told when `seconds` (each) pass without input on `seat`.
    pub fn watch(&mut self, seconds: &[u32], seat: Option<&WlSeat>, qh: &QueueHandle<App>) {
        let seconds: Vec<u32> = match seat {
            Some(_) => seconds.to_vec(),
            None => Vec::new(),
        };
        self.notifications.retain(|n| {
            let keep = seconds.contains(&n.seconds);
            if !keep {
                n.proxy.destroy();
            }
            keep
        });
        for &s in &seconds {
            if let Some(seat) = seat
                && !self.notifications.iter().any(|n| n.seconds == s)
            {
                let proxy =
                    self.notifier
                        .get_idle_notification(s.saturating_mul(1000), seat, qh, ());
                self.notifications.push(Notification {
                    proxy,
                    seconds: s,
                    idle: false,
                });
            }
        }
    }

    /// The longest idle time reached, 0 while there is input.
    pub fn idle_for(&self) -> u32 {
        self.notifications
            .iter()
            .filter(|n| n.idle)
            .map(|n| n.seconds)
            .max()
            .unwrap_or(0)
    }
}

impl Dispatch<ExtIdleNotifierV1, ()> for App {
    fn event(
        _: &mut Self,
        _: &ExtIdleNotifierV1,
        _: <ExtIdleNotifierV1 as smithay_client_toolkit::reexports::client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtIdleNotificationV1, ()> for App {
    fn event(
        app: &mut Self,
        proxy: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(idle) = &mut app.idle else { return };
        let Some(n) = idle.notifications.iter_mut().find(|n| n.proxy == *proxy) else {
            return;
        };
        n.idle = match event {
            ext_idle_notification_v1::Event::Idled => true,
            ext_idle_notification_v1::Event::Resumed => false,
            _ => return,
        };
        app.signals_changed();
    }
}
