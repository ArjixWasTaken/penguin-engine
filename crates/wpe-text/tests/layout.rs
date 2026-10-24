//! Layout and rasterisation against installed system fonts. Tests that need a
//! font family the host lacks return early instead of failing.

use std::sync::{Mutex, MutexGuard, OnceLock};

use wpe_text::{HAlign, POINT_TO_UNITS, TextImage, TextRenderer, TextStyle, VAlign};
use wpe_vfs::Vfs;

fn renderer() -> MutexGuard<'static, TextRenderer> {
    static R: OnceLock<Mutex<TextRenderer>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(TextRenderer::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn has_family(name: &str) -> bool {
    renderer()
        .font_db_mut()
        .faces()
        .any(|f| f.families.iter().any(|(n, _)| n.eq_ignore_ascii_case(name)))
}

fn sans() -> Option<TextStyle> {
    let ok = [
        "Liberation Sans",
        "Arimo",
        "DejaVu Sans",
        "Noto Sans",
        "Arial",
    ]
    .iter()
    .any(|f| has_family(f));
    ok.then(|| TextStyle {
        font: "systemfont_arial".into(),
        pointsize: 12.0,
        padding: [10.0, 6.0],
        ..TextStyle::default()
    })
}

fn render(style: &TextStyle, text: &str) -> TextImage {
    renderer().render(&Vfs::new(), style, text).unwrap()
}

/// Inclusive ink bounds (x0, y0, x1, y1) of texels with alpha >= 128 inside rows `ys`.
fn ink(img: &TextImage, ys: std::ops::Range<u32>) -> Option<(u32, u32, u32, u32)> {
    let mut b: Option<(u32, u32, u32, u32)> = None;
    for y in ys {
        for x in 0..img.width {
            if img.pixels[((y * img.width + x) * 4 + 3) as usize] >= 128 {
                b = Some(match b {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                });
            }
        }
    }
    b
}

fn row_band(img: &TextImage, style: &TextStyle, line: u32) -> std::ops::Range<u32> {
    let pad = style.padding[1] as u32;
    let line_h = (img.height - 2 * pad) / img.lines as u32;
    pad + line * line_h..pad + (line + 1) * line_h
}

#[test]
fn single_line_card_is_text_plus_padding() {
    let Some(style) = sans() else { return };
    let img = render(&style, "Hello");
    assert_eq!(img.lines, 1);
    assert_eq!(img.pixels.len(), (img.width * img.height * 4) as usize);
    assert_eq!(img.card_size, [img.width as f32, img.height as f32]);
    assert_eq!(img.anchor_offset, [0.0, 0.0]);
    let em = 12.0 * POINT_TO_UNITS;
    let content_h = img.height as f32 - 12.0;
    assert!(
        content_h > em && content_h < em * 1.6,
        "{content_h} vs em {em}"
    );
    let (x0, y0, x1, y1) = ink(&img, 0..img.height).expect("glyphs drawn");
    assert!(
        x0 >= 10 && x1 < img.width - 10,
        "ink {x0}..{x1} in {}",
        img.width
    );
    assert!(y0 >= 6 && y1 < img.height - 6);
    assert!(
        x0 <= 14 && x1 >= img.width - 16,
        "line fills the content box"
    );
}

#[test]
fn alignment_pins_card_edges_to_origin() {
    let Some(base) = sans() else { return };
    let cases = [
        (HAlign::Left, VAlign::Top, [0.5, -0.5]),
        (HAlign::Right, VAlign::Bottom, [-0.5, 0.5]),
        (HAlign::Center, VAlign::Top, [0.0, -0.5]),
    ];
    for (halign, valign, k) in cases {
        let style = TextStyle {
            halign,
            valign,
            ..base.clone()
        };
        let img = render(&style, "Edge");
        assert_eq!(
            img.anchor_offset,
            [k[0] * img.card_size[0], k[1] * img.card_size[1]]
        );
    }
}

#[test]
fn multi_line_alignment() {
    let Some(base) = sans() else { return };
    let text = "a much longer first line\nshort\nmid length";
    for halign in [HAlign::Left, HAlign::Center, HAlign::Right] {
        let style = TextStyle {
            halign,
            ..base.clone()
        };
        let img = render(&style, text);
        assert_eq!(img.lines, 3);
        let long = ink(&img, row_band(&img, &style, 0)).unwrap();
        let short = ink(&img, row_band(&img, &style, 1)).unwrap();
        assert!(short.2 - short.0 < long.2 - long.0);
        match halign {
            HAlign::Left => assert!(short.0.abs_diff(long.0) <= 3),
            HAlign::Right => assert!(short.2.abs_diff(long.2) <= 3),
            HAlign::Center => {
                assert!((short.0 + short.2).abs_diff(long.0 + long.2) <= 6)
            }
        }
    }
    let two = render(&base, "x\ny");
    let three = render(&base, "x\ny\nz");
    let line = three.height - two.height;
    assert_eq!(two.height - 12, 2 * line);
}

#[test]
fn trailing_spaces_do_not_widen_or_shift() {
    let Some(base) = sans() else { return };
    let style = TextStyle {
        halign: HAlign::Right,
        ..base
    };
    assert_eq!(
        render(&style, "word").width,
        render(&style, "word   ").width
    );
}

#[test]
fn max_width_wraps_and_max_rows_truncates() {
    let Some(base) = sans() else { return };
    let text = "the quick brown fox jumps over the lazy dog";
    let wrapped = TextStyle {
        max_width: Some(200.0),
        ..base.clone()
    };
    let img = render(&wrapped, text);
    assert!(img.lines >= 3, "{} lines", img.lines);
    assert!(img.card_size[0] <= 200.0 + 20.0 + 1.0);

    let limited = TextStyle {
        max_rows: Some(1),
        ..wrapped.clone()
    };
    assert_eq!(render(&limited, text).lines, 1);

    let ellipsis = TextStyle {
        ellipsis: true,
        ..limited
    };
    let img = render(&ellipsis, text);
    assert_eq!(img.lines, 1);
    assert!(img.card_size[0] <= 221.0);

    let rows = TextStyle {
        max_rows: Some(2),
        ..base
    };
    assert_eq!(render(&rows, "a\nb\nc\nd").lines, 2);
}

#[test]
fn cjk_falls_back_to_system_font() {
    let Some(base) = sans() else { return };
    let cjk = [
        "Noto Sans CJK JP",
        "Noto Sans CJK SC",
        "Source Han Sans",
        "WenQuanYi Zen Hei",
        "Droid Sans Fallback",
    ];
    if !cjk.iter().any(|f| has_family(f)) {
        return;
    }
    let img = render(&base, "漢字かな");
    let (x0, _, x1, _) = ink(&img, 0..img.height).expect("CJK glyphs drawn");
    let em = 12.0 * POINT_TO_UNITS;
    assert!((x1 - x0) as f32 > 3.0 * em, "four full-width glyphs");
}

#[test]
fn background_and_colour_are_baked() {
    let Some(base) = sans() else { return };
    let style = TextStyle {
        color: [1.0, 0.0, 0.0],
        background: Some([0.0, 0.0, 1.0]),
        ..base
    };
    let img = render(&style, "Hi");
    assert_eq!(&img.pixels[..4], &[0, 0, 255, 255]);
    assert!(img.pixels.chunks(4).all(|p| p[3] == 255));
    assert!(img.pixels.chunks(4).any(|p| p[0] == 255 && p[2] == 0));
}

#[test]
fn empty_text_keeps_one_line_card() {
    let Some(style) = sans() else { return };
    let img = render(&style, "");
    assert_eq!(img.lines, 1);
    assert_eq!(img.width, 20);
    assert!(img.height > 12);
    assert!(img.pixels.chunks(4).all(|p| p[3] == 0));
}

#[test]
fn raster_scale_supersamples_same_card() {
    let Some(base) = sans() else { return };
    let x1 = render(&base, "Scale");
    let x2 = render(
        &TextStyle {
            raster_scale: 2.0,
            ..base
        },
        "Scale",
    );
    assert!(x2.width.abs_diff(2 * x1.width) <= 2);
    assert!((x2.card_size[0] - x1.card_size[0]).abs() <= 1.0);
    assert_eq!(x2.raster_scale, 2.0);
}
