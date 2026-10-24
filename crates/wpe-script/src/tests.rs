use std::collections::HashMap;

use serde_json::json;

use super::*;

#[derive(Default)]
struct MockHost {
    layers: Vec<LayerInfo>,
    props: HashMap<(LayerId, String), ScriptValue>,
    writes: Vec<(LayerId, String, ScriptValue)>,
    logs: Vec<String>,
    created: Vec<serde_json::Value>,
    spectrum: f32,
    /// Layer 1 has a video; calls made on it.
    video_calls: Vec<(String, Vec<ScriptValue>)>,
}

impl MockHost {
    fn with_layers(names: &[&str]) -> Self {
        let layers = names
            .iter()
            .enumerate()
            .map(|(i, n)| LayerInfo {
                id: i as LayerId + 1,
                name: (*n).into(),
                effects: vec!["shake".into()],
                ..Default::default()
            })
            .collect();
        Self {
            layers,
            ..Default::default()
        }
    }

    fn prop(&self, layer: LayerId, prop: &str) -> Option<&ScriptValue> {
        self.props.get(&(layer, prop.to_owned()))
    }
}

impl SceneHost for MockHost {
    fn layers(&self) -> Vec<LayerInfo> {
        self.layers.clone()
    }
    fn get(&self, layer: LayerId, property: &str) -> Option<ScriptValue> {
        self.props.get(&(layer, property.to_owned())).cloned()
    }
    fn set(&mut self, layer: LayerId, property: &str, value: ScriptValue) {
        self.writes
            .push((layer, property.to_owned(), value.clone()));
        self.props.insert((layer, property.to_owned()), value);
    }
    fn create_layer(&mut self, config: &serde_json::Value) -> Option<LayerId> {
        self.created.push(config.clone());
        let id = 100 + self.created.len() as LayerId;
        self.layers.push(LayerInfo {
            id,
            name: format!("created{id}"),
            ..Default::default()
        });
        Some(id)
    }
    fn invoke(
        &mut self,
        layer: LayerId,
        target: &str,
        method: &str,
        args: &[ScriptValue],
    ) -> Option<ScriptValue> {
        if target != "video" || layer != 1 {
            return None;
        }
        self.video_calls.push((method.to_owned(), args.to_vec()));
        match method {
            "exists" => Some(ScriptValue::Bool(true)),
            "duration" => Some(ScriptValue::Number(8.0)),
            _ => None,
        }
    }
    fn canvas_size(&self) -> [f32; 2] {
        [1920.0, 1080.0]
    }
    fn audio_spectrum(&self, left: &mut [f32], right: &mut [f32]) {
        left.fill(self.spectrum);
        right.fill(self.spectrum / 2.0);
    }
    fn log(&mut self, _level: LogLevel, message: &str) {
        self.logs.push(message.to_owned());
    }
}

fn add(
    rt: &mut ScriptRuntime,
    host: &mut MockHost,
    layer: LayerId,
    prop: &str,
    src: &str,
    sp: Option<serde_json::Value>,
    initial: ScriptValue,
) -> ScriptId {
    host.props.insert((layer, prop.to_owned()), initial.clone());
    rt.add_property_script(host, layer, prop, src, sp.as_ref(), initial)
        .unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn clock_text_updates() {
    let mut host = MockHost::with_layers(&["clock"]);
    let mut rt = ScriptRuntime::new().unwrap();
    let src = r#"
        'use strict';
        export var scriptProperties = createScriptProperties()
            .addCheckbox({ name: 'use24h', label: '24h', value: true })
            .finish();
        export function update(value) {
            let d = new Date(2024, 0, 2, 13, 5, 9);
            let h = scriptProperties.use24h ? d.getHours() : d.getHours() % 12;
            return ('0' + h).slice(-2) + ':' + ('0' + d.getMinutes()).slice(-2);
        }
    "#;
    let id = add(
        &mut rt,
        &mut host,
        1,
        "text",
        src,
        Some(json!({ "use24h": false })),
        ScriptValue::String("00:00".into()),
    );
    rt.update(&mut host, 0.0, 0.016, None);
    assert_eq!(
        host.prop(1, "text"),
        Some(&ScriptValue::String("01:05".into()))
    );
    assert_eq!(rt.value(id), Some(&ScriptValue::String("01:05".into())));
    assert!(rt.take_failures().is_empty());
}

#[test]
fn text_numbers_stringify_and_undefined_keeps() {
    let mut host = MockHost::with_layers(&["t"]);
    let mut rt = ScriptRuntime::new().unwrap();
    let src = "let n = 0; export function update(v) { n++; return n === 1 ? 42 : undefined; }";
    add(
        &mut rt,
        &mut host,
        1,
        "text",
        src,
        None,
        ScriptValue::String("x".into()),
    );
    rt.update(&mut host, 0.0, 0.016, None);
    rt.update(&mut host, 0.1, 0.016, None);
    assert_eq!(
        host.prop(1, "text"),
        Some(&ScriptValue::String("42".into()))
    );
}

#[test]
fn visibility_toggles_with_runtime() {
    let mut host = MockHost::with_layers(&["blink"]);
    let mut rt = ScriptRuntime::new().unwrap();
    let src = "export function update(value) { return Math.floor(engine.runtime) % 2 === 0; }";
    add(
        &mut rt,
        &mut host,
        1,
        "visible",
        src,
        None,
        ScriptValue::Bool(true),
    );
    rt.update(&mut host, 1.5, 0.016, None);
    assert_eq!(host.prop(1, "visible"), Some(&ScriptValue::Bool(false)));
    rt.update(&mut host, 2.5, 0.016, None);
    assert_eq!(host.prop(1, "visible"), Some(&ScriptValue::Bool(true)));
    let visible_writes = host.writes.iter().filter(|w| w.1 == "visible").count();
    rt.update(&mut host, 2.6, 0.016, None);
    assert_eq!(
        host.writes.iter().filter(|w| w.1 == "visible").count(),
        visible_writes,
        "unchanged results are not written"
    );
}

#[test]
fn origin_uses_canvas_size_and_vec_math() {
    let mut host = MockHost::with_layers(&["dot"]);
    let mut rt = ScriptRuntime::new().unwrap();
    let src = r#"
        import * as WEMath from 'WEMath';
        export function update(value) {
            let c = engine.canvasSize.multiply(0.5);
            return new Vec3(c.x, WEMath.mix(0, c.y, 0.5), value.z);
        }
    "#;
    add(
        &mut rt,
        &mut host,
        1,
        "origin",
        src,
        None,
        ScriptValue::Vec3([0.0, 0.0, 7.0]),
    );
    rt.update(&mut host, 0.0, 0.016, None);
    assert_eq!(
        host.prop(1, "origin"),
        Some(&ScriptValue::Vec3([960.0, 270.0, 7.0]))
    );
}

#[test]
fn scalar_return_broadcasts_to_vector() {
    let mut host = MockHost::with_layers(&["s"]);
    let mut rt = ScriptRuntime::new().unwrap();
    add(
        &mut rt,
        &mut host,
        1,
        "scale",
        "export function update(v) { return v.x + 1; }",
        None,
        ScriptValue::Vec3([1.0; 3]),
    );
    rt.update(&mut host, 0.0, 0.016, None);
    assert_eq!(host.prop(1, "scale"), Some(&ScriptValue::Vec3([2.0; 3])));
}

#[test]
fn script_properties_overridden_by_scene_and_user_bindings() {
    let mut host = MockHost::with_layers(&["l"]);
    let mut rt = ScriptRuntime::new().unwrap();
    rt.set_user_properties(&json!({
        "speed": { "type": "slider", "value": 7 },
        "tint": { "type": "color", "value": "1 0 0" },
    }));
    let src = r#"
        export var scriptProperties = createScriptProperties()
            .addSlider({ name: 'speed', label: 'Speed', value: 1, min: 0, max: 10 })
            .addText({ name: 'label', label: 'Label', value: 'default' })
            .addColor({ name: 'tint', label: 'Tint', value: new Vec3(0, 0, 1) })
            .addCombo({ name: 'mode', label: 'Mode', options: [{ label: 'A', value: 'a' }, { label: 'B', value: 'b' }] })
            .finish();
        export function update(value) {
            return [scriptProperties.speed, scriptProperties.label, scriptProperties.tint.r,
                    scriptProperties.tint instanceof Vec3, scriptProperties.mode].join(',');
        }
    "#;
    let sp = json!({
        "speed": { "user": "speed", "value": 3 },
        "label": "scene",
        "tint": { "user": "tint", "value": "0 1 0" },
    });
    add(
        &mut rt,
        &mut host,
        1,
        "text",
        src,
        Some(sp),
        ScriptValue::String(String::new()),
    );
    rt.update(&mut host, 0.0, 0.016, None);
    assert_eq!(
        host.prop(1, "text"),
        Some(&ScriptValue::String("7,scene,1,true,a".into()))
    );
    rt.apply_user_properties(&mut host, &json!({ "speed": { "value": 9 } }));
    rt.update(&mut host, 0.1, 0.016, None);
    assert_eq!(
        host.prop(1, "text"),
        Some(&ScriptValue::String("9,scene,1,true,a".into()))
    );
}

#[test]
fn apply_user_properties_reaches_scripts() {
    let mut host = MockHost::with_layers(&["a", "b"]);
    let mut rt = ScriptRuntime::new().unwrap();
    let src = r#"
        export function applyUserProperties(changed) {
            if (changed.showb !== undefined) thisScene.getLayer('b').visible = changed.showb;
        }
    "#;
    add(
        &mut rt,
        &mut host,
        1,
        "visible",
        src,
        None,
        ScriptValue::Bool(true),
    );
    rt.init(&mut host);
    rt.apply_user_properties(
        &mut host,
        &json!({ "showb": { "type": "bool", "value": false } }),
    );
    assert_eq!(host.prop(2, "visible"), Some(&ScriptValue::Bool(false)));
}

#[test]
fn cursor_click_toggles_other_layer() {
    let mut host = MockHost::with_layers(&["button", "panel"]);
    let mut rt = ScriptRuntime::new().unwrap();
    let src = r#"
        export function cursorClick(event) {
            let panel = thisScene.getLayer('panel');
            panel.visible = !panel.visible;
            shared.lastClick = event.worldPosition;
        }
    "#;
    host.props
        .insert((2, "visible".into()), ScriptValue::Bool(true));
    add(
        &mut rt,
        &mut host,
        1,
        "visible",
        src,
        None,
        ScriptValue::Bool(true),
    );
    rt.init(&mut host);
    assert_eq!(rt.interactive_layers(), vec![1]);
    let ev = |kind, hits| CursorEvent::new(kind, [10.0, 20.0], [10.0, 1060.0], hits);
    rt.cursor_event(&mut host, ev(CursorEventKind::Down, vec![1]));
    rt.cursor_event(&mut host, ev(CursorEventKind::Up, vec![1]));
    assert_eq!(host.prop(2, "visible"), Some(&ScriptValue::Bool(false)));
    rt.cursor_event(&mut host, ev(CursorEventKind::Down, vec![1]));
    rt.cursor_event(&mut host, ev(CursorEventKind::Up, vec![]));
    assert_eq!(
        host.prop(2, "visible"),
        Some(&ScriptValue::Bool(false)),
        "releasing elsewhere is not a click"
    );
}

#[test]
fn cursor_enter_leave_from_moves() {
    let mut host = MockHost::with_layers(&["hover"]);
    let mut rt = ScriptRuntime::new().unwrap();
    let src = r#"
        export function cursorEnter() { thisLayer.alpha = 1; }
        export function cursorLeave() { thisLayer.alpha = 0.25; }
    "#;
    add(
        &mut rt,
        &mut host,
        1,
        "visible",
        src,
        None,
        ScriptValue::Bool(true),
    );
    let mv = |hits| CursorEvent::new(CursorEventKind::Move, [0.0; 2], [0.0; 2], hits);
    rt.cursor_event(&mut host, mv(vec![1]));
    assert_eq!(host.prop(1, "alpha"), Some(&ScriptValue::Number(1.0)));
    rt.cursor_event(&mut host, mv(vec![]));
    assert_eq!(host.prop(1, "alpha"), Some(&ScriptValue::Number(0.25)));
}

#[test]
fn init_runs_once_and_sets_value() {
    let mut host = MockHost::with_layers(&["l"]);
    let mut rt = ScriptRuntime::new().unwrap();
    let src = r#"
        let count = 0;
        export function init(value) { count++; return 0.5; }
        export function update(value) { return value + count; }
    "#;
    add(
        &mut rt,
        &mut host,
        1,
        "alpha",
        src,
        None,
        ScriptValue::Number(1.0),
    );
    rt.update(&mut host, 0.0, 0.016, None);
    rt.update(&mut host, 0.1, 0.016, None);
    assert_eq!(host.prop(1, "alpha"), Some(&ScriptValue::Number(2.5)));
}

#[test]
fn errors_disable_without_panicking() {
    let mut host = MockHost::with_layers(&["l"]);
    let mut rt = ScriptRuntime::new().unwrap();
    let id = add(
        &mut rt,
        &mut host,
        1,
        "alpha",
        "export function update(v) { return undefinedThing.x; }",
        None,
        ScriptValue::Number(1.0),
    );
    rt.update(&mut host, 0.0, 0.016, None);
    assert!(rt.is_disabled(id));
    let f = rt.take_failures();
    assert_eq!(f.len(), 1);
    assert!(f[0].message.contains("ReferenceError"), "{}", f[0].message);
    rt.update(&mut host, 0.1, 0.016, None);
    assert!(rt.take_failures().is_empty());

    let err = rt
        .add_property_script(
            &mut host,
            1,
            "alpha",
            "export function (",
            None,
            ScriptValue::Number(1.0),
        )
        .unwrap_err();
    assert!(matches!(err, ScriptError::Compile(_)), "{err}");
}

#[test]
fn runaway_scripts_are_interrupted() {
    let mut host = MockHost::with_layers(&["l"]);
    let mut rt = ScriptRuntime::with_options(ScriptOptions {
        call_budget: Duration::from_millis(20),
        ..Default::default()
    })
    .unwrap();
    let id = add(
        &mut rt,
        &mut host,
        1,
        "alpha",
        "export function update(v) { while (true) {} }",
        None,
        ScriptValue::Number(1.0),
    );
    let start = Instant::now();
    rt.update(&mut host, 0.0, 0.016, None);
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(rt.is_disabled(id));
    assert!(rt.take_failures()[0].timeout);
    assert!(matches!(
        rt.add_property_script(
            &mut host,
            1,
            "alpha",
            "while (true) {}",
            None,
            ScriptValue::Number(1.0)
        ),
        Err(ScriptError::Timeout)
    ));
}

#[test]
fn timers_fire_on_runtime_clock() {
    let mut host = MockHost::with_layers(&["l"]);
    let mut rt = ScriptRuntime::new().unwrap();
    let src = r#"
        let n = 0;
        export function init() {
            engine.setTimeout(() => { thisLayer.text = 'fired'; }, 500);
            const stop = setInterval(() => { n++; if (n === 3) stop(); }, 100);
        }
        export function update(v) { return String(n); }
    "#;
    add(
        &mut rt,
        &mut host,
        1,
        "pointsize",
        src,
        None,
        ScriptValue::String(String::new()),
    );
    for f in 0..=12 {
        rt.update(&mut host, f as f64 * 0.1, 0.1, None);
    }
    assert_eq!(
        host.prop(1, "text"),
        Some(&ScriptValue::String("fired".into()))
    );
    assert_eq!(
        host.prop(1, "pointsize"),
        Some(&ScriptValue::String("3".into()))
    );
}

#[test]
fn layers_by_index_effects_and_create_layer() {
    let mut host = MockHost::with_layers(&["a", "b", "c"]);
    let mut rt = ScriptRuntime::new().unwrap();
    let src = r#"
        export function init() {
            for (let i = 0; i < thisScene.getLayerCount(); i++) {
                const l = thisScene.getLayer(i);
                if (l.name === 'c') l.origin = new Vec2(5, 6);
            }
            thisScene.getLayer('b').getEffect('shake').visible = false;
            thisScene.getLayer('a').getMaterial().setValue('g_Speed', 2);
            const bar = thisScene.createLayer({ image: 'models/bar.json', origin: new Vec3(1, 2, 3) });
            bar.alpha = 0.5;
            shared.sameProxy = thisScene.getLayer('a') === thisLayer;
        }
        export function update(v) { return shared.sameProxy && thisScene.getLayerIndex('c') === 2; }
    "#;
    add(
        &mut rt,
        &mut host,
        1,
        "visible",
        src,
        None,
        ScriptValue::Bool(false),
    );
    rt.update(&mut host, 0.0, 0.016, None);
    assert!(rt.take_failures().is_empty());
    assert_eq!(
        host.prop(3, "origin"),
        Some(&ScriptValue::Vec3([5.0, 6.0, 0.0]))
    );
    assert_eq!(
        host.prop(2, "effects/0/visible"),
        Some(&ScriptValue::Bool(false))
    );
    assert_eq!(
        host.prop(1, "material/g_Speed"),
        Some(&ScriptValue::Number(2.0))
    );
    assert_eq!(
        host.created[0],
        json!({ "image": "models/bar.json", "origin": "1 2 3" })
    );
    assert_eq!(host.prop(101, "alpha"), Some(&ScriptValue::Number(0.5)));
    assert_eq!(host.prop(1, "visible"), Some(&ScriptValue::Bool(true)));
}

#[test]
fn audio_buffers_refresh_each_frame() {
    let mut host = MockHost::with_layers(&["bars"]);
    host.spectrum = 0.5;
    let mut rt = ScriptRuntime::new().unwrap();
    let src = r#"
        const audio = engine.registerAudioBuffers(engine.AUDIO_RESOLUTION_16);
        export function update(v) { return audio.left.length + audio.left[3] + audio.average[0]; }
    "#;
    add(
        &mut rt,
        &mut host,
        1,
        "alpha",
        src,
        None,
        ScriptValue::Number(0.0),
    );
    rt.update(&mut host, 0.0, 0.016, None);
    assert_eq!(host.prop(1, "alpha"), Some(&ScriptValue::Number(16.875)));
}

#[test]
fn console_goes_to_host_log() {
    let mut host = MockHost::with_layers(&["l"]);
    let mut rt = ScriptRuntime::new().unwrap();
    add(
        &mut rt,
        &mut host,
        1,
        "visible",
        "console.log('hello', new Vec2(1, 2), { a: 1 });",
        None,
        ScriptValue::Bool(true),
    );
    assert_eq!(host.logs, vec!["hello 1 2 {\"a\":1}".to_owned()]);
}

#[test]
fn video_texture_controls_and_ended_callbacks() {
    let mut host = MockHost::with_layers(&["video", "still"]);
    let mut rt = ScriptRuntime::new().unwrap();
    let src = r#"
        let ended = 0, still, video;
        export function init() {
            video = thisScene.getLayer('video').getVideoTexture();
            still = thisScene.getLayer('still').getVideoTexture();
            video.loop = false;
            video.setCurrentTime(video.duration / 2);
            video.play();
            video.addEndedCallback(() => { ended++; });
        }
        export function update() {
            return (still === null) + ' ' + ended;
        }
    "#;
    add(
        &mut rt,
        &mut host,
        2,
        "text",
        src,
        None,
        ScriptValue::String(String::new()),
    );
    rt.update(&mut host, 0.0, 0.016, None);
    assert_eq!(
        host.prop(2, "text"),
        Some(&ScriptValue::String("true 0".into()))
    );
    let calls: Vec<&str> = host.video_calls.iter().map(|(m, _)| m.as_str()).collect();
    assert_eq!(
        calls,
        ["exists", "setLoop", "duration", "setCurrentTime", "play"]
    );
    assert_eq!(host.video_calls[3].1, [ScriptValue::Number(4.0)]);

    rt.video_ended(&mut host, 1);
    rt.update(&mut host, 0.1, 0.016, None);
    assert_eq!(
        host.prop(2, "text"),
        Some(&ScriptValue::String("true 1".into()))
    );
    assert!(rt.take_failures().is_empty());
}
