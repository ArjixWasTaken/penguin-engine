//! Telling systemd how the service is doing (`sd_notify`); all of it does
//! nothing outside a `Type=notify` unit.

pub use sd_notify::NotifyState;
use smithay_client_toolkit::reexports::calloop::LoopHandle;
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};

pub fn notify(states: &[NotifyState]) {
    if let Err(e) = sd_notify::notify(states) {
        tracing::debug!("sd_notify: {e}");
    }
}

pub fn status(text: &str) {
    notify(&[NotifyState::Status(text)]);
}

/// Ping the watchdog from the loop at half its interval, when the unit has
/// `WatchdogSec`.
pub fn start_watchdog<D: 'static>(handle: &LoopHandle<'static, D>) {
    let Some(interval) = sd_notify::watchdog_enabled() else {
        return;
    };
    let period = interval / 2;
    tracing::info!("systemd watchdog every {period:?}");
    let inserted = handle.insert_source(Timer::from_duration(period), move |_, _, _| {
        notify(&[NotifyState::Watchdog]);
        TimeoutAction::ToDuration(period)
    });
    if let Err(e) = inserted {
        tracing::warn!("cannot arm the watchdog timer: {e}");
    }
}
