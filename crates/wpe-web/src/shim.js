// Wallpaper Engine's web wallpaper API, run before the page's own scripts.
// `__WPE_INIT__` is replaced with {properties, general, paused}.
(() => {
  const init = __WPE_INIT__;
  const properties = init.properties || {};
  let general = init.general || {};
  let paused = !!init.paused;
  let listener;
  let delivered = false;
  const audioListeners = [];

  const call = (name, ...args) => {
    const f = listener && listener[name];
    if (typeof f !== "function") return;
    try {
      f.apply(listener, args);
    } catch (e) {
      console.error(`wallpaperPropertyListener.${name}: ${e}\n${e && e.stack}`);
    }
  };

  // The first delivery is everything, once the page has loaded (as
  // Wallpaper Engine does); a listener set later gets it right away.
  const deliver = () => {
    delivered = true;
    call("applyUserProperties", properties);
    call("applyGeneralProperties", general);
    if (paused) call("setPaused", true);
  };
  const loaded = () => document.readyState === "complete";
  window.addEventListener("load", () => setTimeout(() => listener && deliver(), 0));

  Object.defineProperty(window, "wallpaperPropertyListener", {
    configurable: true,
    get: () => listener,
    set: (value) => {
      listener = value;
      if (loaded() || delivered) setTimeout(deliver, 0);
    },
  });

  window.wallpaperRegisterAudioListener = (callback) => {
    if (typeof callback === "function") audioListeners.push(callback);
  };
  for (const name of [
    "wallpaperRegisterMediaStatusListener",
    "wallpaperRegisterMediaPropertiesListener",
    "wallpaperRegisterMediaThumbnailListener",
    "wallpaperRegisterMediaPlaybackListener",
    "wallpaperRegisterMediaTimelineListener",
  ]) {
    window[name] = () => {};
  }
  window.wallpaperRequestRandomFileForProperty = () => {};

  const media = () => document.querySelectorAll("video, audio");
  const resumable = new WeakSet();

  window.__wpe = {
    apply(changed) {
      Object.assign(properties, changed);
      call("applyUserProperties", changed);
    },
    general(changed) {
      general = Object.assign({}, general, changed);
      call("applyGeneralProperties", changed);
    },
    setPaused(value) {
      if (paused === value) return;
      paused = value;
      for (const m of media()) {
        if (value && !m.paused) {
          resumable.add(m);
          m.pause();
        } else if (!value && resumable.has(m)) {
          resumable.delete(m);
          m.play().catch(() => {});
        }
      }
      call("setPaused", value);
    },
    audio(samples) {
      for (const callback of audioListeners) {
        try {
          callback(samples);
        } catch (e) {
          console.error(`audio listener: ${e}`);
        }
      }
    },
    audioRegistered: () => audioListeners.length > 0,
  };
})();
