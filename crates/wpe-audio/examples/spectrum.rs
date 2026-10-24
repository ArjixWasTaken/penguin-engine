//! Print the peak of the system audio spectrum for a few seconds.

fn main() {
    let mut capture = wpe_audio::AudioCapture::start().expect("audio capture");
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let s = capture.spectrum();
        let peak = s
            .left64
            .iter()
            .chain(&s.right64)
            .fold(0.0f32, |a, &b| a.max(b));
        println!("peak {peak:.3}");
    }
}
