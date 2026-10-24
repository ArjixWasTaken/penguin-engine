//! Open a web wallpaper directory and save frames: `probe <dir> <out.png> [seconds]`.
use std::time::{Duration, Instant};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("wallpaper dir"));
    let out = args.next().expect("output png");
    let seconds: f32 = args.next().map_or(3.0, |s| s.parse().unwrap());
    let project: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("project.json")).unwrap()).unwrap();
    let file = project["file"].as_str().unwrap_or("index.html").to_owned();
    let properties = project["general"]["properties"].clone();
    let name = dir.file_name().unwrap().to_string_lossy().into_owned();
    let started = Instant::now();
    let options = wpe_web::PageOptions {
        root: dir,
        file,
        name,
        size: (1920, 1080),
        scale: 1.0,
        properties,
        general: serde_json::json!({ "fps": 30 }),
        network: Default::default(),
        profile_dir: None,
        export_frames: false,
    };
    // `PROBE_TWO`: a second page alongside (as on a second output).
    let second = std::env::var_os("PROBE_TWO").map(|_| {
        let mut o = options.clone();
        o.size = (1280, 720);
        wpe_web::WebPage::open(o, Duration::from_secs(20)).unwrap()
    });
    let mut page = wpe_web::WebPage::open(options, Duration::from_secs(20)).unwrap();
    println!("ready={} after {:?}", page.ready(), started.elapsed());
    if std::env::var_os("PROBE_EVENTS").is_some() {
        use wpe_web::{Button, Input};
        std::thread::sleep(Duration::from_millis(500));
        page.input(Input::Move(100.0, 200.0));
        for button in [Button::Left, Button::Right, Button::Middle] {
            page.input(Input::Button(button, true, 100.0, 200.0));
            page.input(Input::Button(button, false, 100.0, 200.0));
        }
        page.input(Input::Wheel {
            dx: 0.0,
            dy: 120.0,
            x: 100.0,
            y: 200.0,
        });
        page.apply_properties(&serde_json::json!({"color": {"type": "color", "value": "0 0 1"}}));
        page.set_paused(true);
        std::thread::sleep(Duration::from_millis(1500));
        page.set_paused(false);
        page.set_scale(2.0);
    }
    // `PROBE_CLICKS=n`: n left clicks in the middle, 150 ms apart.
    if let Some(n) = std::env::var("PROBE_CLICKS")
        .ok()
        .and_then(|n| n.parse::<u32>().ok())
    {
        use wpe_web::{Button, Input};
        std::thread::sleep(Duration::from_millis(500));
        if let Ok(js) = std::env::var("PROBE_JS") {
            page.eval(js);
        }
        let (x, y) = (960.0, 540.0);
        page.input(Input::Move(x, y));
        for _ in 0..n {
            page.input(Input::Button(Button::Left, true, x, y));
            page.input(Input::Button(Button::Left, false, x, y));
            std::thread::sleep(Duration::from_millis(150));
        }
    }
    let deadline = Instant::now() + Duration::from_secs_f32(seconds);
    let mut frames = 0;
    let mut last = None;
    while Instant::now() < deadline {
        if std::env::var_os("PROBE_TICK").is_some() {
            page.tick();
            std::thread::sleep(Duration::from_millis(28));
        }
        if let Some(f) = page.take_frame() {
            frames += 1;
            last = Some(f);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    println!("{frames} frames in {seconds}s");
    if let Some(second) = &second {
        let f = second.take_frame();
        println!("second page: {:?}", f.map(|f| (f.width, f.height)));
    }
    if let Some(f) = last {
        image::save_buffer(
            &out,
            &f.pixels,
            f.width,
            f.height,
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
    }
    drop(page);
    drop(second);
    wpe_web::shutdown(Duration::from_secs(3));
}
