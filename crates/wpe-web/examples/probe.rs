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
    let page = wpe_web::WebPage::open(
        wpe_web::PageOptions {
            root: dir,
            file,
            name,
            size: (1920, 1080),
            scale: 1.0,
            properties,
            general: serde_json::json!({ "fps": 30 }),
            network: Default::default(),
            profile_dir: None,
        },
        Duration::from_secs(20),
    )
    .unwrap();
    println!("ready={} after {:?}", page.ready(), started.elapsed());
    let deadline = Instant::now() + Duration::from_secs_f32(seconds);
    let mut frames = 0;
    let mut last = None;
    while Instant::now() < deadline {
        if let Some(f) = page.take_frame() {
            frames += 1;
            last = Some(f);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    println!("{frames} frames in {seconds}s");
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
    wpe_web::shutdown(Duration::from_secs(3));
}
