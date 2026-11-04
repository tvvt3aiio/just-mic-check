#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Glyph {
    App,
    Stopped,
    Live,
}

type Rgb = [f32; 3];

const fn rgb(r: u8, g: u8, b: u8) -> Rgb {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0]
}

const INK: Rgb = rgb(0xFF, 0xFF, 0xFF);
const APP_BODY: Rgb = rgb(0x22, 0x30, 0x2B);
const STOPPED_BODY: Rgb = rgb(0x6B, 0x7A, 0x73);
const LIVE_BODY: Rgb = rgb(0x1F, 0x8F, 0x6E);
const LED_RED: Rgb = rgb(0xF0, 0x56, 0x4A);
const LED_AMBER: Rgb = rgb(0xF4, 0xB7, 0x40);
const LED_GREEN: Rgb = rgb(0x6F, 0xDD, 0xA0);
const APP_DETAIL_MIN: u32 = 32;

pub fn render(glyph: Glyph, size: u32) -> Vec<u8> {
    let mut c = Canvas::new(size);
    if glyph == Glyph::App && size >= APP_DETAIL_MIN {
        c.fill(APP_BODY, |x, y| rounded_box(x, y, 0.0, 0.0, 64.0, 64.0, 14.0));
        c.mic(Mic { cx: 21.0, half_w: 8.0, top: 10.0, bottom: 37.0, arc_y: 27.0, arc_r: 15.0, stroke: 5.0, stem: (42.0, 54.0) });
        for (y, color) in [(10.0, LED_RED), (21.0, LED_AMBER), (32.0, LED_GREEN), (43.0, LED_GREEN)] {
            c.fill(color, |px, py| rounded_box(px, py, 44.0, y, 56.0, y + 8.0, 2.0));
        }
    } else {
        let body = match glyph {
            Glyph::App => APP_BODY,
            Glyph::Stopped => STOPPED_BODY,
            Glyph::Live => LIVE_BODY,
        };
        c.fill(body, |x, y| rounded_box(x, y, 0.0, 0.0, 64.0, 64.0, 15.0));
        c.mic(Mic { cx: 32.0, half_w: 9.0, top: 9.0, bottom: 39.0, arc_y: 29.0, arc_r: 18.0, stroke: 6.0, stem: (47.0, 56.0) });
    }
    c.into_rgba()
}

struct Mic {
    cx: f32,
    half_w: f32,
    top: f32,
    bottom: f32,
    arc_y: f32,
    arc_r: f32,
    stroke: f32,
    stem: (f32, f32),
}

struct Canvas {
    size: u32,
    px: Vec<[f32; 4]>,
}

impl Canvas {
    fn new(size: u32) -> Self {
        Self { size, px: vec![[0.0; 4]; (size * size) as usize] }
    }

    fn fill(&mut self, color: Rgb, sdf: impl Fn(f32, f32) -> f32) {
        let scale = self.size as f32 / 64.0;
        for y in 0..self.size {
            for x in 0..self.size {
                let d = sdf((x as f32 + 0.5) / scale, (y as f32 + 0.5) / scale);
                let a = (0.5 - d * scale).clamp(0.0, 1.0);
                if a > 0.0 {
                    let p = &mut self.px[(y * self.size + x) as usize];
                    for i in 0..3 {
                        p[i] = color[i] * a + p[i] * (1.0 - a);
                    }
                    p[3] = a + p[3] * (1.0 - a);
                }
            }
        }
    }

    fn mic(&mut self, m: Mic) {
        self.fill(INK, |x, y| rounded_box(x, y, m.cx - m.half_w, m.top, m.cx + m.half_w, m.bottom, m.half_w));
        self.fill(INK, |x, y| lower_arc(x, y, m.cx, m.arc_y, m.arc_r) - m.stroke / 2.0);
        self.fill(INK, |x, y| segment(x, y, (m.cx, m.stem.0), (m.cx, m.stem.1)) - m.stroke / 2.0);
    }

    fn into_rgba(self) -> Vec<u8> {
        let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        self.px
            .iter()
            .flat_map(|p| {
                let a = p[3].max(1e-6);
                [byte(p[0] / a), byte(p[1] / a), byte(p[2] / a), byte(p[3])]
            })
            .collect()
    }
}

pub fn rounded_box(x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> f32 {
    let dx = (x0 + r - x).max(x - (x1 - r));
    let dy = (y0 + r - y).max(y - (y1 - r));
    dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0) - r
}

fn segment(x: f32, y: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let t = (((x - a.0) * dx + (y - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    (x - a.0 - t * dx).hypot(y - a.1 - t * dy)
}

fn lower_arc(x: f32, y: f32, cx: f32, cy: f32, r: f32) -> f32 {
    if y >= cy {
        ((x - cx).hypot(y - cy) - r).abs()
    } else {
        (x - (cx - r)).hypot(y - cy).min((x - (cx + r)).hypot(y - cy))
    }
}
