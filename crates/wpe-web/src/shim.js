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

  // Directory properties come with what they hold (`files`), handed to
  // `userDirectoryFilesAddedOrChanged` rather than kept in the values.
  const directories = {};
  const takeDirectories = (props) => {
    const changed = [];
    for (const [name, entry] of Object.entries(props)) {
      if (entry && Array.isArray(entry.files)) {
        directories[name] = entry.files;
        delete entry.files;
        changed.push(name);
      }
    }
    return changed;
  };
  const announceDirectories = (names) => {
    for (const name of names) call("userDirectoryFilesAddedOrChanged", name, directories[name]);
  };
  const initialDirectories = takeDirectories(properties);

  // The first delivery is everything, once the page has loaded (as
  // Wallpaper Engine does); a listener set later gets it right away.
  const deliver = () => {
    delivered = true;
    call("applyUserProperties", properties);
    call("applyGeneralProperties", general);
    announceDirectories(initialDirectories);
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
    if (typeof callback !== "function") return;
    audioListeners.push(callback);
    if (audioListeners.length === 1) fetch("/__wpe/rpc/audio-registered").catch(() => {});
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
  window.wallpaperRequestRandomFileForProperty = (name, callback) => {
    const files = directories[name] || [];
    if (typeof callback !== "function" || files.length === 0) return;
    const file = files[Math.floor(Math.random() * files.length)];
    setTimeout(() => callback(name, file), 0);
  };

  // Page sound goes through a master volume (0 while audio is off): media
  // elements' volume is scaled, Web Audio passes a gain before the output.
  let master = 0;
  const volumeOf = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, "volume");
  const wanted = new WeakMap();
  const played = new Set();
  const applyVolume = (m) => volumeOf.set.call(m, (wanted.has(m) ? wanted.get(m) : 1) * master);
  if (volumeOf && volumeOf.set) {
    Object.defineProperty(HTMLMediaElement.prototype, "volume", {
      configurable: true,
      get() {
        return wanted.has(this) ? wanted.get(this) : volumeOf.get.call(this);
      },
      set(v) {
        wanted.set(this, v);
        applyVolume(this);
      },
    });
    const play = HTMLMediaElement.prototype.play;
    HTMLMediaElement.prototype.play = function (...args) {
      played.add(this);
      applyVolume(this);
      return play.apply(this, args);
    };
    document.addEventListener("play", (e) => { played.add(e.target); applyVolume(e.target); }, true);
  }
  const gains = new Set();
  if (window.AudioNode && window.AudioDestinationNode) {
    const connect = AudioNode.prototype.connect;
    const gainFor = new WeakMap();
    AudioNode.prototype.connect = function (destination, ...rest) {
      if (destination instanceof AudioDestinationNode) {
        let gain = gainFor.get(destination);
        if (!gain) {
          gain = destination.context.createGain();
          gain.gain.value = master;
          connect.call(gain, destination);
          gainFor.set(destination, gain);
          gains.add(gain);
        }
        return connect.call(this, gain, ...rest);
      }
      return connect.call(this, destination, ...rest);
    };
  }

  const media = () => document.querySelectorAll("video, audio");
  const resumable = new WeakSet();

  window.__wpe = {
    apply(changed) {
      const dirs = takeDirectories(changed);
      Object.assign(properties, changed);
      call("applyUserProperties", changed);
      announceDirectories(dirs);
    },
    setVolume(volume) {
      master = volume;
      if (volumeOf && volumeOf.set) {
        for (const m of document.querySelectorAll("video, audio")) played.add(m);
        for (const m of played) applyVolume(m);
      }
      for (const g of gains) g.gain.value = volume;
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
