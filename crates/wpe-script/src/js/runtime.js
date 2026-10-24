// SceneScript globals (engine, thisScene, input, console, localStorage, shared,
// timers) and the layer proxies. Everything that touches the scene goes
// through `__native`, which forwards to the Rust `SceneHost`.
(function (global, N) {
	'use strict';
	const W = {
		scripts: new Map(),
		proxies: new Map(),
		layerList: null,
		nextStub: -2,
		timers: [],
		nextTimer: 1,
		audioRegs: [],
		now: 0,
	};
	Object.defineProperty(global, '__wpe', { value: W });
	delete global.__native;

	W.vec = (n, x, y, z, w) => (n === 2 ? new Vec2(x, y) : n === 3 ? new Vec3(x, y, z) : new Vec4(x, y, z, w));

	function stringify(obj) {
		return JSON.stringify(obj, (k, v) => (v && typeof v.toConfigString === 'function' ? v.toConfigString() : v));
	}
	W.stringify = stringify;

	function describe(a) {
		if (typeof a === 'string') return a;
		if (a instanceof Error) return a.name + ': ' + a.message;
		if (a === null || typeof a !== 'object') return String(a);
		if (typeof a.toString === 'function' && a.toString !== Object.prototype.toString && !Array.isArray(a)) return String(a);
		try { return stringify(a); } catch (e) { return String(a); }
	}
	function logger(level) {
		return function (...args) { N.log(level, args.map(describe).join(' ')); };
	}
	global.console = { log: logger(0), info: logger(0), debug: logger(0), warn: logger(1), error: logger(2), trace: logger(0) };

	// ---------------------------------------------------------------- layers
	function layerInfos() {
		if (W.layerList === null) W.layerList = N.layers();
		return W.layerList;
	}
	W.invalidateLayers = () => { W.layerList = null; };
	function infoById(id) {
		const list = layerInfos();
		for (let i = 0; i < list.length; i++) if (list[i].id === id) return list[i];
		return null;
	}
	function indexOfId(id) {
		const list = layerInfos();
		for (let i = 0; i < list.length; i++) if (list[i].id === id) return i;
		return -1;
	}
	function resolveId(x) {
		if (x instanceof Layer) return x.__id;
		const list = layerInfos();
		if (typeof x === 'number') { const l = list[x | 0]; return l ? l.id : null; }
		if (typeof x === 'string') {
			for (let i = 0; i < list.length; i++) if (list[i].name === x) return list[i].id;
		}
		return null;
	}
	function proxy(id) {
		if (id === null || id === undefined) return null;
		let p = W.proxies.get(id);
		if (!p) { p = new Layer(id); W.proxies.set(id, p); }
		return p;
	}
	W.layer = proxy;

	const DEFAULTS = {
		origin: () => new Vec3(0, 0, 0), scale: () => new Vec3(1, 1, 1), angles: () => new Vec3(0, 0, 0),
		color: () => new Vec3(1, 1, 1), backgroundcolor: () => new Vec3(0, 0, 0), alpha: () => 1, visible: () => true,
		text: () => '', pointsize: () => 0, parallaxDepth: () => new Vec2(0, 0), size: () => new Vec2(0, 0),
		volume: () => 1, perspective: () => false, solid: () => true, font: () => '', horizontalalign: () => 'center',
		verticalalign: () => 'center', alignment: () => 'center', anchor: () => 'none', padding: () => 0,
	};
	const KINDS = {
		origin: 'v3', scale: 'v3', angles: 'v3', color: 'v3', backgroundcolor: 'v3', parallaxDepth: 'v2',
		alpha: 'n', pointsize: 'n', padding: 'n', maxrows: 'n', maxwidth: 'n', volume: 'n', fov: 'n', zoom: 'n',
		visible: 'b', perspective: 'b', solid: 'b', limitrows: 'b', limitwidth: 'b', opaquebackground: 'b', rootmotion: 'b',
		text: 's', font: 's', horizontalalign: 's', verticalalign: 's', alignment: 's', anchor: 's',
	};
	function coerce(kind, v, current) {
		switch (kind) {
			case 'v3':
				if (v instanceof Vec3) return v;
				if (typeof v === 'number' || typeof v === 'string') return new Vec3(v);
				if (v && typeof v === 'object') {
					const z = typeof v.z === 'number' ? v.z : (current && typeof current.z === 'number' ? current.z : 0);
					return new Vec3(+v.x || 0, +v.y || 0, z);
				}
				return v;
			case 'v2':
				if (v instanceof Vec2) return v;
				if (typeof v === 'number' || typeof v === 'string') return new Vec2(v);
				if (v && typeof v === 'object') return new Vec2(+v.x || 0, +v.y || 0);
				return v;
			case 'n': return typeof v === 'number' ? v : Number(v);
			case 'b': return !!v;
			case 's': return v === null || v === undefined ? '' : String(v);
		}
		return v;
	}
	function hostGet(id, prop) {
		return id >= 0 ? N.get(id, prop) : undefined;
	}

	class Layer {
		constructor(id) {
			Object.defineProperty(this, '__id', { value: id });
			Object.defineProperty(this, '__shadow', { value: {} });
			Object.defineProperty(this, '__initial', { value: {}, writable: true });
			Object.defineProperty(this, '__video', { value: { ended: [] } });
		}
		_read(prop) {
			const v = hostGet(this.__id, prop);
			if (v !== undefined && v !== null) return v;
			if (prop in this.__shadow) return this.__shadow[prop];
			return DEFAULTS[prop] ? DEFAULTS[prop]() : undefined;
		}
		_write(prop, v) {
			const kind = KINDS[prop];
			const c = kind ? coerce(kind, v, kind === 'v3' ? this._read(prop) : undefined) : v;
			this.__shadow[prop] = c;
			if (this.__id >= 0) N.set(this.__id, prop, c);
		}
		_invoke(target, method, args, fallback) {
			if (this.__id < 0) return fallback;
			const r = N.invoke(this.__id, target, method, args || []);
			return r === undefined || r === null ? fallback : r;
		}
		_captureInitial() {
			if (this.__initial.origin) return;
			this.__initial = { origin: this.origin, scale: this.scale, angles: this.angles, alpha: this.alpha,
				visible: this.visible, color: this.color, size: this.size };
		}
		get name() { const i = infoById(this.__id); return i ? i.name : (this.__shadow.name || ''); }
		get id() { return this.__id; }
		get debug() { return undefined; }
		get size() { return this._read('size'); }
		get opacity() { return this.alpha; }
		set opacity(v) { this.alpha = v; }
		get originalOrigin() { this._captureInitial(); return this.__initial.origin.copy(); }
		get originalScale() { this._captureInitial(); return this.__initial.scale.copy(); }
		get originalAngles() { this._captureInitial(); return this.__initial.angles.copy(); }
		get worldOrigin() { return this._read('worldOrigin') || this.origin; }
		get worldScale() { return this._read('worldScale') || this.scale; }
		isObjectValid() { return this.__id >= 0 && infoById(this.__id) !== null; }
		getInitialLayerConfig() {
			this._captureInitial();
			const i = this.__initial;
			return { name: this.name, origin: i.origin.copy(), scale: i.scale.copy(), angles: i.angles.copy(),
				alpha: i.alpha, visible: i.visible, color: i.color.copy(), size: i.size };
		}
		getTransformMatrix() {
			const m = hostGet(this.__id, 'transform');
			if (Array.isArray(m) && m.length === 16) return new Mat4(m.map(Number));
			return Mat4.compose(this.origin, this.angles, this.scale);
		}
		rotateObjectSpace(a) { this._invoke('', 'rotateObjectSpace', [a]); }
		lookAt(c, up) { this._invoke('', 'lookAt', [c, up || new Vec3(0, 1, 0)]); }
		lookAtYaw(c, up) { this._invoke('', 'lookAtYaw', [c, up || new Vec3(0, 1, 0)]); }

		getParent() { const i = infoById(this.__id); return i && i.parent !== null && i.parent !== undefined ? proxy(i.parent) : null; }
		getChildren() { return layerInfos().filter(l => l.parent === this.__id).map(l => proxy(l.id)); }
		setParent(parent) {
			const pid = parent === null || parent === undefined ? -1 : resolveId(parent);
			if (pid === null || pid === this.__id || this.__id < 0) return;
			N.parent(this.__id, pid);
			W.invalidateLayers();
		}
		removeFromParent() { this.setParent(null); }
		getAttachmentIndex(name) { return this._invoke('', 'getAttachmentIndex', [String(name)], -1); }
		getAttachmentMatrix(a) { return new Mat4(this._invoke('', 'getAttachmentMatrix', [a], undefined)); }
		getAttachmentOrigin(a) { return this._invoke('', 'getAttachmentOrigin', [a], this.origin); }
		getAttachmentAngles(a) { return this._invoke('', 'getAttachmentAngles', [a], this.angles); }
		transformAttachmentToTexture() { return new Mat3(); }

		play() { this._invoke('', 'play'); }
		pause() { this._invoke('', 'pause'); }
		stop() { this._invoke('', 'stop'); }
		isPlaying() { return !!this._invoke('', 'isPlaying', [], false); }
		emitParticles(count) { this._invoke('', 'emitParticles', [count === undefined ? 1 : +count]); }
		get instance() { return particleInstance(this); }

		getEffectCount() { const i = infoById(this.__id); return i ? i.effects.length : 0; }
		getEffect(key) {
			const i = infoById(this.__id);
			if (!i) return null;
			let idx = -1;
			if (typeof key === 'number') idx = key >= 0 && key < i.effects.length ? key | 0 : -1;
			else idx = i.effects.indexOf(String(key));
			if (idx < 0) return null;
			if (!this.__effects) Object.defineProperty(this, '__effects', { value: new Map() });
			let e = this.__effects.get(idx);
			if (!e) { e = makeEffect(this, idx, i.effects[idx]); this.__effects.set(idx, e); }
			return e;
		}
		getMaterial() { return makeMaterial(this, 'material/'); }

		getTextureAnimation() { return makeAnimation(this, 'textureAnimation', ''); }
		getAnimation(name) {
			return makeAnimation(this, name === undefined || name === null ? 'animation' : 'animation:' + name, name || '');
		}
		getAnimationLayerCount() { return this._invoke('', 'getAnimationLayerCount', [], 0); }
		getAnimationLayer(key) { return makeAnimation(this, 'animationLayer:' + key, String(key)); }
		createAnimationLayer(anim, cfg) {
			const r = this._invoke('', 'createAnimationLayer', [anim, cfg || {}], null);
			return r === null ? null : makeAnimation(this, 'animationLayer:' + r, String(r));
		}
		playSingleAnimation(anim, cfg) { return this.createAnimationLayer(anim, cfg); }
		destroyAnimationLayer(l) { return !!this._invoke('', 'destroyAnimationLayer', [l && l.name !== undefined ? l.name : l], false); }
		getVideoTexture() { return makeVideo(this); }

		getBoneCount() { return this._invoke('', 'getBoneCount', [], 0); }
		getBoneIndex(name) { return this._invoke('', 'getBoneIndex', [String(name)], -1); }
		getBoneParentIndex(b) { return this._invoke('', 'getBoneParentIndex', [b], -1); }
		getBoneTransform(b) { return new Mat4(this._invoke('', 'getBoneTransform', [b], undefined)); }
		setBoneTransform(b, m) { this._invoke('', 'setBoneTransform', [b, m && m.m ? m.m : m]); }
		getLocalBoneTransform(b) { return new Mat4(this._invoke('', 'getLocalBoneTransform', [b], undefined)); }
		setLocalBoneTransform(b, m) { this._invoke('', 'setLocalBoneTransform', [b, m && m.m ? m.m : m]); }
		getLocalBoneAngles(b) { return this._invoke('', 'getLocalBoneAngles', [b], new Vec3(0, 0, 0)); }
		setLocalBoneAngles(b, v) { this._invoke('', 'setLocalBoneAngles', [b, v]); }
		getLocalBoneOrigin(b) { return this._invoke('', 'getLocalBoneOrigin', [b], new Vec3(0, 0, 0)); }
		setLocalBoneOrigin(b, v) { this._invoke('', 'setLocalBoneOrigin', [b, v]); }
		applyBonePhysicsImpulse(b, d, a) { this._invoke('', 'applyBonePhysicsImpulse', [b, d, a]); }
		resetBonePhysicsSimulation(b) { this._invoke('', 'resetBonePhysicsSimulation', [b]); }
		getBlendShapeIndex(n) { return this._invoke('', 'getBlendShapeIndex', [String(n)], -1); }
		getBlendShapeWeight(b) { return this._invoke('', 'getBlendShapeWeight', [b], 0); }
		setBlendShapeWeight(b, w) { this._invoke('', 'setBlendShapeWeight', [b, +w]); }
	}
	for (const prop of Object.keys(KINDS)) {
		Object.defineProperty(Layer.prototype, prop, {
			get() { return this._read(prop); },
			set(v) { this._write(prop, v); },
			enumerable: true, configurable: true,
		});
	}

	function makeAnimation(layer, target, name) {
		const call = (m, args, fb) => layer._invoke(target, m, args, fb);
		return {
			get name() { return name; },
			get fps() { return call('fps', [], 30); },
			get frameCount() { return call('frameCount', [], 1); },
			get duration() { return call('duration', [], 0); },
			get rate() { return call('rate', [], 1); },
			set rate(v) { call('setRate', [+v]); },
			get blend() { return call('blend', [], 1); },
			set blend(v) { call('setBlend', [+v]); },
			get visible() { return call('visible', [], true); },
			set visible(v) { call('setVisible', [!!v]); },
			play() { call('play'); },
			pause() { call('pause'); },
			stop() { call('stop'); },
			isPlaying() { return !!call('isPlaying', [], false); },
			getFrame() { return call('getFrame', [], 0); },
			setFrame(f) { call('setFrame', [+f]); },
			addEndedCallback(cb) { if (typeof cb === 'function') layer.__video.ended.push(cb); },
			join() {},
		};
	}
	function makeVideo(layer) {
		const call = (m, args, fb) => layer._invoke('video', m, args, fb);
		return {
			get duration() { return call('duration', [], 0); },
			get rate() { return call('rate', [], 1); },
			set rate(v) { call('setRate', [+v || 1]); },
			get loop() { return !!call('loop', [], true); },
			set loop(v) { call('setLoop', [!!v]); },
			play() { call('play'); },
			pause() { call('pause'); },
			stop() { call('stop'); },
			isPlaying() { return !!call('isPlaying', [], false); },
			getCurrentTime() { return call('getCurrentTime', [], 0); },
			setCurrentTime(t) { call('setCurrentTime', [+t || 0]); },
			addEndedCallback(cb) { if (typeof cb === 'function') layer.__video.ended.push(cb); },
		};
	}
	function packMaterial(v) {
		if (Array.isArray(v)) {
			const n = v.map(Number);
			if (n.length === 1) return n[0];
			if (n.length >= 2 && n.length <= 4) return W.vec(n.length, n[0], n[1], n[2], n[3]);
			return n;
		}
		return v;
	}
	function makeMaterial(layer, prefix) {
		const cache = {};
		const m = {
			setValue(name, v) {
				name = String(name || '');
				if (!name) return;
				v = packMaterial(v);
				cache[name] = v;
				if (layer.__id >= 0) N.set(layer.__id, prefix + name, v);
			},
			getValue(name) {
				name = String(name || '');
				const v = hostGet(layer.__id, prefix + name);
				return v !== undefined && v !== null ? v : (name in cache ? cache[name] : null);
			},
			getAnimation(name) { return makeAnimation(layer, prefix + 'animation:' + (name || ''), name || ''); },
		};
		for (const alias of ['color', 'alpha', 'channelMask', 'tint']) {
			Object.defineProperty(m, alias, { get() { return m.getValue(alias); }, set(v) { m.setValue(alias, v); }, enumerable: true });
		}
		return m;
	}
	function makeEffect(layer, idx, name) {
		const base = 'effects/' + idx + '/';
		return {
			get name() { return name; },
			get visible() {
				const v = hostGet(layer.__id, base + 'visible');
				return v === undefined || v === null ? true : !!v;
			},
			set visible(v) { if (layer.__id >= 0) N.set(layer.__id, base + 'visible', !!v); },
			getMaterial(i) { return makeMaterial(layer, base + 'material/'); },
			getMaterialCount() { return 1; },
			setMaterialProperty(p, v) { if (layer.__id >= 0) N.set(layer.__id, base + 'material/' + p, packMaterial(v)); },
			executeMaterialFunction(p) { layer._invoke('effect:' + idx, 'executeMaterialFunction', [String(p)]); },
			getAnimation(n) { return makeAnimation(layer, base + 'animation:' + (n || ''), n || ''); },
		};
	}
	const PARTICLE_PROPS = ['alpha', 'size', 'count', 'speed', 'lifetime', 'rate', 'colorn', 'controlpoint0', 'controlpoint1',
		'controlpoint2', 'controlpoint3', 'controlpoint4', 'controlpoint5', 'controlpoint6', 'controlpoint7'];
	function particleInstance(layer) {
		const o = {};
		for (const p of PARTICLE_PROPS) {
			const key = 'instanceoverride/' + p;
			Object.defineProperty(o, p, {
				get() {
					const v = hostGet(layer.__id, key);
					return v !== undefined && v !== null ? v : (p.startsWith('controlpoint') ? new Vec3(0, 0, 0) : 1);
				},
				set(v) { if (layer.__id >= 0) N.set(layer.__id, key, v); },
				enumerable: true,
			});
		}
		return o;
	}
	function nullLayer(config) {
		const p = new Layer(W.nextStub--);
		if (config && typeof config === 'object') {
			for (const k of ['origin', 'scale', 'angles', 'alpha', 'color', 'visible', 'text']) if (k in config) p[k] = config[k];
			if (config.name) p.__shadow.name = String(config.name);
		}
		return p;
	}

	// ----------------------------------------------------------------- scene
	const SCENE_PROPS = ['bloom', 'bloomstrength', 'bloomthreshold', 'clearenabled', 'clearcolor', 'ambientcolor',
		'skylightcolor', 'fov', 'nearz', 'farz', 'camerafade', 'camerashake', 'camerashakespeed', 'camerashakeamplitude',
		'camerashakeroughness', 'cameraparallax', 'cameraparallaxamount', 'cameraparallaxdelay', 'cameraparallaxmouseinfluence'];
	const sceneShadow = {};
	const thisScene = {
		getLayer(x) { return proxy(resolveId(x)); },
		getLayerByID(id) { const n = Number(id); return infoById(n) ? proxy(n) : null; },
		getLayerCount() { return layerInfos().length; },
		enumerateLayers() { return layerInfos().map(l => proxy(l.id)); },
		getLayerIndex(x) { const id = resolveId(x); return id === null ? -1 : indexOfId(id); },
		sortLayer(x, index) {
			const id = resolveId(x);
			if (id === null || id < 0) return false;
			N.sort(id, Math.max(0, index | 0));
			W.invalidateLayers();
			return true;
		},
		createLayer(config) {
			let desc;
			if (typeof config === 'string') desc = config;
			else if (config && config.__asset !== undefined) desc = config.__asset;
			else desc = JSON.parse(stringify(config || {}));
			const id = N.create(stringify(desc));
			W.invalidateLayers();
			if (typeof id !== 'number' || id < 0) return nullLayer(config);
			return proxy(id);
		},
		destroyLayer(x) {
			const id = resolveId(x);
			if (id === null || id < 0) return false;
			N.destroy(id);
			W.proxies.delete(id);
			W.invalidateLayers();
			return true;
		},
		getInitialLayerConfig(x) { const l = x instanceof Layer ? x : proxy(resolveId(x)); return l ? l.getInitialLayerConfig() : null; },
		getCameraTransforms() {
			const t = N.sceneGet('cameratransforms');
			if (t && typeof t === 'object') {
				return { eye: new Vec3(t.eye), center: new Vec3(t.center), up: new Vec3(t.up), zoom: +t.zoom || 1 };
			}
			return { eye: new Vec3(0, 0, 1), center: new Vec3(0, 0, 0), up: new Vec3(0, 1, 0), zoom: 1 };
		},
		setCameraTransforms(t) { N.sceneSet('cameratransforms', JSON.parse(stringify(t))); },
		getAnimation(name) { return makeAnimation(new Layer(-1), 'scene', name || ''); },
		createModelData() { return { applyData() {}, replaceData() {}, toConfigString() { return ''; } }; },
		destroyModelData() {},
	};
	for (const prop of SCENE_PROPS) {
		Object.defineProperty(thisScene, prop, {
			get() {
				const v = N.sceneGet(prop);
				return v !== undefined && v !== null ? v : sceneShadow[prop];
			},
			set(v) { sceneShadow[prop] = v; N.sceneSet(prop, v); },
			enumerable: true,
		});
	}
	global.thisScene = thisScene;

	// ---------------------------------------------------------------- engine
	const engine = {
		frametime: 0, runtime: 0, timeOfDay: 0, frameCount: 0,
		canvasSize: new Vec2(1920, 1080), screenResolution: new Vec2(1920, 1080),
		userProperties: {},
		AUDIO_RESOLUTION_16: 16, AUDIO_RESOLUTION_32: 32, AUDIO_RESOLUTION_64: 64,
		colorScheme: new Vec3(1, 1, 1),
		isRunningInEditor: () => false,
		isPortrait() { return this.screenResolution.y > this.screenResolution.x; },
		isLandscape() { return this.screenResolution.y <= this.screenResolution.x; },
		isDesktopDevice: () => true,
		isMobileDevice: () => false,
		isTabletDevice: () => false,
		isWallpaper: () => true,
		isScreensaver: () => false,
		getUserProperty(name) { return this.userProperties[name]; },
		openUserShortcut: () => false,
		registerAsset(file) { return { __asset: String(file), toConfigString() { return this.__asset; } }; },
		registerAudioBuffers(resolution) {
			const r = +resolution || 64;
			const n = r <= 24 ? 16 : r <= 48 ? 32 : 64;
			const buf = { left: new Float32Array(n), right: new Float32Array(n), average: new Float32Array(n), resolution: n };
			W.audioRegs.push(buf);
			return buf;
		},
	};
	global.engine = engine;
	global.shared = {};
	global.input = { cursorWorldPosition: new Vec3(0, 0, 0), cursorScreenPosition: new Vec2(0, 0), cursorLeftDown: false };

	const storage = { global: {}, screen: {} };
	const loc = l => (l === 'global' || l === 0 ? 'global' : 'screen');
	global.localStorage = {
		LOCATION_GLOBAL: 'global', LOCATION_SCREEN: 'screen',
		get(k, l) { const s = storage[loc(l)]; return Object.prototype.hasOwnProperty.call(s, k) ? s[k] : undefined; },
		set(k, v, l) { storage[loc(l)][String(k)] = v; },
		delete(k, l) { const s = storage[loc(l)]; const had = Object.prototype.hasOwnProperty.call(s, k); delete s[k]; return had; },
		remove(k, l) { return this.delete(k, l); },
		clear(l) { if (l === undefined) { storage.global = {}; storage.screen = {}; } else storage[loc(l)] = {}; },
	};

	// ---------------------------------------------------------------- timers
	function addTimer(fn, delay, repeat) {
		const id = W.nextTimer++;
		const ms = Math.max(0, +delay || 0);
		if (typeof fn === 'function') W.timers.push({ id, fn, ms, repeat, due: W.now + ms / 1000 });
		const cancel = () => clearTimer(id);
		cancel.__timerId = id;
		return cancel;
	}
	function clearTimer(h) {
		const id = h && h.__timerId !== undefined ? h.__timerId : h;
		W.timers = W.timers.filter(t => t.id !== id);
	}
	global.setTimeout = (fn, d) => addTimer(fn, d, false);
	global.setInterval = (fn, d) => addTimer(fn, d, true);
	global.clearTimeout = clearTimer;
	global.clearInterval = clearTimer;
	engine.setTimeout = global.setTimeout;
	engine.setInterval = global.setInterval;
	engine.clearTimeout = clearTimer;
	engine.clearInterval = clearTimer;

	W.runTimers = function () {
		const due = W.timers.filter(t => t.due <= W.now);
		if (due.length === 0) return;
		due.sort((a, b) => a.due - b.due || a.id - b.id);
		for (const t of due) {
			if (t.repeat) t.due = Math.max(t.due + Math.max(t.ms, 1) / 1000, W.now);
			else W.timers = W.timers.filter(x => x !== t);
			try { t.fn(); } catch (e) { N.log(2, 'timer callback: ' + describe(e)); }
		}
	};

	// ----------------------------------------------------------------- frame
	W.frame = function (time, frametime, cw, cs, ldown, canvas, screen) {
		W.now = time;
		engine.runtime = time;
		engine.frametime = frametime;
		engine.frameCount++;
		const d = new Date();
		engine.timeOfDay = (d.getHours() * 3600 + d.getMinutes() * 60 + d.getSeconds() + d.getMilliseconds() / 1000) / 86400;
		engine.canvasSize.x = canvas[0]; engine.canvasSize.y = canvas[1];
		engine.screenResolution.x = screen[0]; engine.screenResolution.y = screen[1];
		if (cw) {
			const i = global.input;
			i.cursorWorldPosition.x = cw[0]; i.cursorWorldPosition.y = cw[1]; i.cursorWorldPosition.z = 0;
			i.cursorScreenPosition.x = cs[0]; i.cursorScreenPosition.y = cs[1];
			i.cursorLeftDown = !!ldown;
		}
		for (const buf of W.audioRegs) {
			const n = buf.resolution, a = N.audio(n);
			for (let k = 0; k < n; k++) {
				const l = +a[k] || 0, r = +a[n + k] || 0;
				buf.left[k] = l; buf.right[k] = r; buf.average[k] = (l + r) / 2;
			}
		}
	};

	// --------------------------------------------------------------- scripts
	W.register = function (sid, layerId, property, kind) {
		const layer = layerId === null ? null : proxy(layerId);
		let obj = layer;
		const m = /^effects\/(\d+)\//.exec(property);
		if (layer && m) obj = layer.getEffect(+m[1]) || layer;
		W.scripts.set(sid, { layer, obj, kind, ns: null });
	};
	W.scopeLayer = sid => W.scripts.get(sid).layer;
	W.scopeObject = sid => W.scripts.get(sid).obj;
	const HANDLERS = ['init', 'update', 'destroy', 'applyUserProperties', 'cursorClick', 'cursorDown', 'cursorUp',
		'cursorMove', 'cursorEnter', 'cursorLeave', 'resizeScreen', 'mediaStatusChanged', 'mediaPlaybackChanged',
		'mediaPropertiesChanged', 'mediaThumbnailChanged', 'mediaTimelineChanged', 'applyGeneralSettings'];
	W.attach = function (sid, ns) {
		const e = W.scripts.get(sid);
		e.ns = ns;
		let mask = 0;
		HANDLERS.forEach((h, i) => { if (typeof ns[h] === 'function') mask |= 1 << i; });
		return mask;
	};
	W.applyScriptProperties = function (sid, json) {
		const e = W.scripts.get(sid);
		const sp = e && e.ns && e.ns.scriptProperties;
		if (!sp || typeof sp !== 'object') return;
		const vars = JSON.parse(json);
		for (const key of Object.keys(vars)) {
			if (!Object.prototype.hasOwnProperty.call(sp, key)) continue;
			const cur = sp[key], v = vars[key];
			if (cur instanceof Vec3) sp[key] = new Vec3(v);
			else if (cur instanceof Vec2) sp[key] = new Vec2(v);
			else if (typeof cur === 'number' && typeof v === 'string' && v.trim() !== '' && !isNaN(+v)) sp[key] = +v;
			else sp[key] = v;
		}
	};
	W.call = function (sid, name, arg) {
		const e = W.scripts.get(sid);
		return e.ns[name](arg);
	};
	W.callValue = function (sid, name, value) {
		const e = W.scripts.get(sid);
		const r = e.ns[name](value);
		if (r === undefined || r === null) return undefined;
		if (e.kind === 's') {
			if (typeof r === 'number' && !Number.isFinite(r)) return undefined;
			return String(r);
		}
		return r;
	};
	W.cursorEvent = function (sid, name, wx, wy, sx, sy) {
		const e = W.scripts.get(sid);
		const world = new Vec3(wx, wy, 0);
		const origin = e.layer ? e.layer.origin : new Vec3(0, 0, 0);
		const ev = { worldPosition: world, screenPosition: new Vec2(sx, sy), localPosition: world.subtract(origin), button: 0, x: wx, y: wy };
		return e.ns[name](ev);
	};
	W.videoEnded = function (layerId) {
		const p = W.proxies.get(layerId);
		if (!p) return;
		for (const cb of p.__video.ended.slice()) {
			try { cb(); } catch (e) { N.log(2, 'video ended callback: ' + describe(e)); }
		}
	};
	W.setUserProperties = function (json) {
		const raw = JSON.parse(json);
		const changed = {};
		for (const k of Object.keys(raw)) {
			const e = raw[k];
			let v;
			if (e !== null && typeof e === 'object' && !Array.isArray(e) && 'value' in e) {
				if (e.type === 'color') v = new Vec3(e.value);
				else if (e.type === 'usershortcut') v = { isbound: e.isbound, commandtype: e.commandtype, file: e.file };
				else v = e.value;
			} else {
				v = e;
			}
			changed[k] = v;
			engine.userProperties[k] = v;
		}
		const sc = engine.userProperties.schemecolor;
		if (sc !== undefined && sc !== null) {
			const c = new Vec3(sc);
			c.primary = c.copy();
			engine.colorScheme = c;
		}
		W.changedUserProperties = changed;
	};
	W.callChanged = function (sid, all) {
		return W.scripts.get(sid).ns.applyUserProperties(all ? Object.assign({}, engine.userProperties) : (W.changedUserProperties || {}));
	};
})(globalThis, globalThis.__native);
