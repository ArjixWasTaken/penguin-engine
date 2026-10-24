//! Which other applications play sound, for the rules: the sound server's
//! (PulseAudio or PipeWire's) playing streams, watched on a thread.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use libpulse_binding as pulse;
use pulse::callbacks::ListResult;
use pulse::context::subscribe::InterestMaskSet;
use pulse::context::{Context, FlagSet, State};
use pulse::mainloop::standard::{IterateResult, Mainloop};
use pulse::proplist::properties;
use smithay_client_toolkit::reexports::calloop::channel::Sender;

/// Watch on a thread; `tx` gets the applications playing sound whenever
/// that changes (sorted, without duplicates), or None while the sound
/// server can't be reached.
pub fn watch(tx: Sender<Option<Vec<String>>>) {
    let spawned = std::thread::Builder::new()
        .name("sound-watch".into())
        .spawn(move || {
            let mut last = None;
            loop {
                if let Err(e) = run(&tx, &mut last) {
                    tracing::debug!("sound server: {e}");
                }
                if last.take().is_some() && tx.send(None).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_secs(10));
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("cannot watch the sound server: {e}");
    }
}

/// Connect and report until the connection ends.
fn run(tx: &Sender<Option<Vec<String>>>, last: &mut Option<Vec<String>>) -> Result<(), String> {
    let mut mainloop = Mainloop::new().ok_or("no main loop")?;
    let mut context = Context::new(&mainloop, "wallpaper").ok_or("no context")?;
    context
        .connect(None, FlagSet::NOAUTOSPAWN, None)
        .map_err(|e| e.to_string().unwrap_or_else(|| format!("{e:?}")))?;
    loop {
        iterate(&mut mainloop)?;
        match context.get_state() {
            State::Ready => break,
            State::Failed | State::Terminated => return Err("cannot connect".into()),
            _ => {}
        }
    }
    let dirty = Rc::new(Cell::new(true));
    {
        let dirty = dirty.clone();
        context.set_subscribe_callback(Some(Box::new(move |_, _, _| dirty.set(true))));
    }
    context.subscribe(InterestMaskSet::SINK_INPUT, |_| {});
    let ours = std::process::id().to_string();
    let listing = Rc::new(RefCell::new(None::<Vec<String>>));
    let mut asking = false;
    loop {
        if dirty.get() && !asking {
            dirty.set(false);
            asking = true;
            let listing = listing.clone();
            let ours = ours.clone();
            let mut found = Vec::new();
            context
                .introspect()
                .get_sink_input_info_list(move |r| match r {
                    ListResult::Item(info) => {
                        let props = &info.proplist;
                        let get = |key| props.get_str(key);
                        let playing = !info.corked && !info.mute && !info.volume.is_muted();
                        let event = get(properties::MEDIA_ROLE).as_deref() == Some("event");
                        let own = get(properties::APPLICATION_PROCESS_ID).as_deref() == Some(&ours);
                        if playing && !event && !own {
                            let name = get(properties::APPLICATION_PROCESS_BINARY)
                                .or_else(|| get(properties::APPLICATION_NAME))
                                .unwrap_or_default();
                            found.push(name);
                        }
                    }
                    ListResult::End | ListResult::Error => {
                        let mut found = std::mem::take(&mut found);
                        found.sort();
                        found.dedup();
                        *listing.borrow_mut() = Some(found);
                    }
                });
        }
        if let Some(found) = listing.borrow_mut().take() {
            asking = false;
            if last.as_ref() != Some(&found) {
                tracing::debug!("playing sound: {found:?}");
                *last = Some(found.clone());
                tx.send(Some(found)).map_err(|_| "the daemon is gone")?;
            }
        }
        iterate(&mut mainloop)?;
        if !matches!(context.get_state(), State::Ready) {
            return Err("disconnected".into());
        }
    }
}

fn iterate(mainloop: &mut Mainloop) -> Result<(), String> {
    match mainloop.iterate(true) {
        IterateResult::Success(_) => Ok(()),
        IterateResult::Quit(_) => Err("main loop quit".into()),
        IterateResult::Err(e) => Err(e.to_string().unwrap_or_else(|| format!("{e:?}"))),
    }
}
