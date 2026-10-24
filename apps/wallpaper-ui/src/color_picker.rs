//! A colour picker: a saturation/value square and a hue bar.

use iced::mouse;
use iced::widget::canvas::{self, Event, Frame, Geometry, Path, Stroke, gradient};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Size, Theme};

const HUE_BAR: f32 = 18.0;
const GAP: f32 = 8.0;

pub fn picker<'a, Message: 'a>(
    color: Color,
    on_change: impl Fn(Color) -> Message + 'a,
) -> Element<'a, Message> {
    canvas::Canvas::new(Picker {
        color,
        on_change: Box::new(on_change),
    })
    .width(Length::Fill)
    .height(150)
    .into()
}

struct Picker<'a, Message> {
    color: Color,
    on_change: Box<dyn Fn(Color) -> Message + 'a>,
}

#[derive(Default)]
pub struct State {
    dragging: Option<Zone>,
    /// The last hue and saturation picked, which grey and black lose.
    hsv: Option<[f32; 3]>,
}

#[derive(Clone, Copy)]
enum Zone {
    Square,
    Hue,
}

impl<Message> Picker<'_, Message> {
    fn hsv(&self, state: &State) -> [f32; 3] {
        match state.hsv {
            Some(hsv) if same(hsv_to_rgb(hsv), self.color) => hsv,
            _ => rgb_to_hsv(self.color),
        }
    }
}

fn zones(bounds: Size) -> (Rectangle, Rectangle) {
    let square = Rectangle::new(
        Point::ORIGIN,
        Size::new(bounds.width - HUE_BAR - GAP, bounds.height),
    );
    let hue = Rectangle::new(
        Point::new(bounds.width - HUE_BAR, 0.0),
        Size::new(HUE_BAR, bounds.height),
    );
    (square, hue)
}

impl<Message> canvas::Program<Message> for Picker<'_, Message> {
    type State = State;

    fn update(
        &self,
        state: &mut State,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        let (square, hue) = zones(bounds.size());
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let at = cursor.position_in(bounds)?;
                state.dragging = if square.contains(at) {
                    Some(Zone::Square)
                } else if hue.contains(at) {
                    Some(Zone::Hue)
                } else {
                    return None;
                };
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.dragging.take()?;
                return Some(canvas::Action::capture());
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if state.dragging.is_some() => {}
            _ => return None,
        }
        let at = cursor.land().position()?;
        let at = Point::new(at.x - bounds.x, at.y - bounds.y);
        let mut hsv = self.hsv(state);
        match state.dragging? {
            Zone::Square => {
                hsv[1] = ((at.x - square.x) / square.width).clamp(0.0, 1.0);
                hsv[2] = 1.0 - ((at.y - square.y) / square.height).clamp(0.0, 1.0);
            }
            Zone::Hue => hsv[0] = ((at.y - hue.y) / hue.height).clamp(0.0, 1.0) * 360.0,
        }
        state.hsv = Some(hsv);
        Some(canvas::Action::publish((self.on_change)(hsv_to_rgb(hsv))).and_capture())
    }

    fn draw(
        &self,
        state: &State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let (square, hue) = zones(bounds.size());
        let [h, s, v] = self.hsv(state);

        let pure = hsv_to_rgb([h, 1.0, 1.0]);
        let across = gradient::Linear::new(
            square.position(),
            Point::new(square.x + square.width, square.y),
        )
        .add_stop(0.0, Color::WHITE)
        .add_stop(1.0, pure);
        frame.fill_rectangle(square.position(), square.size(), across);
        let down = gradient::Linear::new(
            square.position(),
            Point::new(square.x, square.y + square.height),
        )
        .add_stop(0.0, Color::TRANSPARENT)
        .add_stop(1.0, Color::BLACK);
        frame.fill_rectangle(square.position(), square.size(), down);

        let mut bar = gradient::Linear::new(hue.position(), Point::new(hue.x, hue.y + hue.height));
        for i in 0..=6 {
            let t = i as f32 / 6.0;
            bar = bar.add_stop(t, hsv_to_rgb([t * 360.0, 1.0, 1.0]));
        }
        frame.fill_rectangle(hue.position(), hue.size(), bar);

        let marker = Point::new(
            square.x + s * square.width,
            square.y + (1.0 - v) * square.height,
        );
        let ring = if v > 0.5 && s < 0.5 {
            Color::BLACK
        } else {
            Color::WHITE
        };
        frame.stroke(
            &Path::circle(marker, 6.0),
            Stroke::default().with_color(ring).with_width(2.0),
        );
        let y = hue.y + h / 360.0 * hue.height;
        frame.stroke(
            &Path::rectangle(
                Point::new(hue.x - 2.0, y - 3.0),
                Size::new(HUE_BAR + 4.0, 6.0),
            ),
            Stroke::default().with_color(Color::WHITE).with_width(2.0),
        );
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _: &State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if cursor.is_over(bounds) {
            mouse::Interaction::Crosshair
        } else {
            mouse::Interaction::default()
        }
    }
}

fn same(a: Color, b: Color) -> bool {
    a.into_rgba8() == b.into_rgba8()
}

/// Hue in degrees, saturation and value 0..1.
pub fn rgb_to_hsv(c: Color) -> [f32; 3] {
    let max = c.r.max(c.g).max(c.b);
    let min = c.r.min(c.g).min(c.b);
    let d = max - min;
    let h = if d == 0.0 {
        0.0
    } else if max == c.r {
        60.0 * ((c.g - c.b) / d).rem_euclid(6.0)
    } else if max == c.g {
        60.0 * ((c.b - c.r) / d + 2.0)
    } else {
        60.0 * ((c.r - c.g) / d + 4.0)
    };
    let s = if max == 0.0 { 0.0 } else { d / max };
    [h, s, max]
}

pub fn hsv_to_rgb([h, s, v]: [f32; 3]) -> Color {
    let c = v * s;
    let h = (h.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    Color::from_rgb(r + m, g + m, b + m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_round_trips() {
        for rgb in [
            [255, 0, 0],
            [0, 255, 0],
            [0, 0, 255],
            [255, 128, 0],
            [10, 20, 30],
            [255, 255, 255],
            [0, 0, 0],
        ] {
            let c = Color::from_rgb8(rgb[0], rgb[1], rgb[2]);
            assert!(same(hsv_to_rgb(rgb_to_hsv(c)), c), "{rgb:?}");
        }
    }
}
