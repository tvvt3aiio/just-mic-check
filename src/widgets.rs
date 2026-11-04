use windows_sys::Win32::Graphics::Gdi::SetPixelV;
use winsafe::{self as w, co, guard::DeleteObjectGuard, COLORREF, POINT, RECT, SIZE};

use crate::icon::rounded_box;

pub const BODY: COLORREF = COLORREF::from_rgb(0xD0, 0xD8, 0xD2);
const FIELD: COLORREF = COLORREF::from_rgb(0xE7, 0xEC, 0xE8);
const FIELD_HOVER: COLORREF = COLORREF::from_rgb(0xF1, 0xF4, 0xF1);
const FIELD_DOWN: COLORREF = COLORREF::from_rgb(0xDC, 0xE3, 0xDE);
pub const EDGE: COLORREF = COLORREF::from_rgb(0x9F, 0xAC, 0xA4);
pub const INK: COLORREF = COLORREF::from_rgb(0x1F, 0x2A, 0x26);
pub const SUB_INK: COLORREF = COLORREF::from_rgb(0x46, 0x55, 0x4E);
const ACCENT: COLORREF = COLORREF::from_rgb(0x1F, 0x6F, 0x5C);
const WHITE: COLORREF = COLORREF::from_rgb(0xFF, 0xFF, 0xFF);
const DISPLAY: COLORREF = COLORREF::from_rgb(0x22, 0x30, 0x2B);
const DISPLAY_TEXT: COLORREF = COLORREF::from_rgb(0xA9, 0xB8, 0xB0);
const LED_OFF: COLORREF = COLORREF::from_rgb(0x34, 0x44, 0x3D);
const LED_GREEN: COLORREF = COLORREF::from_rgb(0x6F, 0xDD, 0xA0);
const LED_AMBER: COLORREF = COLORREF::from_rgb(0xF4, 0xB7, 0x40);
const LED_RED: COLORREF = COLORREF::from_rgb(0xF0, 0x56, 0x4A);

const CORNER: i32 = 8;
const DISPLAY_CORNER: i32 = 10;
pub const ROW_HEIGHT: i32 = 32;
pub const LABEL_WIDTH: i32 = 72;
pub const VALUE_WIDTH: i32 = 64;
pub const PANEL_MARGIN: i32 = 14;
pub const ITEM_SPACING: i32 = 6;
pub const METER_HEIGHT: i32 = 66;
pub const MAIN_BUTTON_HEIGHT: i32 = 44;
pub const WINDOW_WIDTH: i32 = 420;

const METER_SEGMENTS: usize = 36;
const METER_FLOOR_DB: f32 = -60.0;
const METER_SCALE_DB: [i32; 6] = [-60, -48, -36, -24, -12, 0];
const METER_GAP: f32 = 3.0;

const FONT_FACES: [(&str, &str, &str); 4] = [
    (r"C:\Windows\Fonts\BIZ-UDGothicR.ttc", "BIZ UDPGothic", "BIZ UDGothic"),
    (r"C:\Windows\Fonts\YuGothM.ttc", "Yu Gothic Medium", "Yu Gothic Medium"),
    (r"C:\Windows\Fonts\meiryo.ttc", "Meiryo", "Meiryo"),
    (r"C:\Windows\Fonts\msgothic.ttc", "MS PGothic", "MS Gothic"),
];

pub fn scale(value: i32, dpi: u32) -> i32 {
    w::MulDiv(value, dpi as i32, 96)
}

fn scale_f(value: f32, dpi: u32) -> f32 {
    value * dpi as f32 / 96.0
}

pub struct Fonts {
    pub body: DeleteObjectGuard<w::HFONT>,
    pub main: DeleteObjectGuard<w::HFONT>,
    pub number: DeleteObjectGuard<w::HFONT>,
}

fn font(face: &str, height: i32, dpi: u32) -> w::SysResult<DeleteObjectGuard<w::HFONT>> {
    let mut lf = w::LOGFONT::default();
    lf.lfHeight = -scale(height, dpi);
    lf.lfQuality = co::QUALITY::CLEARTYPE;
    lf.lfCharSet = co::CHARSET::DEFAULT;
    lf.set_lfFaceName(face);
    w::HFONT::CreateFontIndirect(&lf)
}

impl Fonts {
    pub fn new(dpi: u32) -> w::SysResult<Self> {
        let (_, ui, number) = FONT_FACES
            .iter()
            .find(|(path, ..)| std::path::Path::new(path).exists())
            .copied()
            .unwrap_or(("", "Segoe UI", "Consolas"));
        Ok(Self { body: font(ui, 14, dpi)?, main: font(ui, 15, dpi)?, number: font(number, 12, dpi)? })
    }
}

pub fn fill(dc: &w::HDC, rc: RECT, color: COLORREF) {
    if let Ok(brush) = w::HBRUSH::CreateSolidBrush(color) {
        let _ = dc.FillRect(rc, &brush);
    }
}

fn blend(fg: COLORREF, bg: COLORREF, a: f32) -> COLORREF {
    let ch = |f: u8, b: u8| (f as f32 * a + b as f32 * (1.0 - a)).round() as u8;
    COLORREF::from_rgb(ch(fg.GetRValue(), bg.GetRValue()), ch(fg.GetGValue(), bg.GetGValue()), ch(fg.GetBValue(), bg.GetBValue()))
}

fn fill_shape(dc: &w::HDC, bounds: RECT, fg: COLORREF, bg: COLORREF, sdf: impl Fn(f32, f32) -> f32) {
    for y in bounds.top..bounds.bottom {
        for x in bounds.left..bounds.right {
            let a = (0.5 - sdf(x as f32 + 0.5, y as f32 + 0.5)).clamp(0.0, 1.0);
            if a > 0.0 {
                unsafe {
                    SetPixelV(dc.ptr() as _, x, y, blend(fg, bg, a).into());
                }
            }
        }
    }
}

pub fn rounded(dc: &w::HDC, rc: RECT, radius: f32, fg: COLORREF, bg: COLORREF) {
    let r = radius.ceil() as i32;
    fill(dc, RECT { left: rc.left + r, right: rc.right - r, ..rc }, fg);
    fill(dc, RECT { right: rc.left + r, top: rc.top + r, bottom: rc.bottom - r, ..rc }, fg);
    fill(dc, RECT { left: rc.right - r, top: rc.top + r, bottom: rc.bottom - r, ..rc }, fg);
    let sdf = |x: f32, y: f32| rounded_box(x, y, rc.left as f32, rc.top as f32, rc.right as f32, rc.bottom as f32, radius);
    for (left, top) in [(rc.left, rc.top), (rc.right - r, rc.top), (rc.left, rc.bottom - r), (rc.right - r, rc.bottom - r)] {
        fill_shape(dc, RECT { left, top, right: left + r, bottom: top + r }, fg, bg, sdf);
    }
}

fn inset(rc: RECT, by: i32) -> RECT {
    RECT { left: rc.left + by, top: rc.top + by, right: rc.right - by, bottom: rc.bottom - by }
}

fn outlined(dc: &w::HDC, rc: RECT, radius: f32, fg: COLORREF, edge: COLORREF, line: i32, bg: COLORREF) {
    rounded(dc, rc, radius, edge, bg);
    rounded(dc, inset(rc, line), (radius - line as f32).max(0.0), fg, edge);
}

pub fn text(dc: &w::HDC, face: &w::HFONT, rc: RECT, s: &str, color: COLORREF, format: co::DT) {
    let Ok(_font) = dc.SelectObject(face) else { return };
    let _ = dc.SetBkMode(co::BKMODE::TRANSPARENT);
    let _ = dc.SetTextColor(color);
    let mut rc = rc;
    let _ = dc.DrawText(s, &mut rc, format | co::DT::NOPREFIX | co::DT::SINGLELINE | co::DT::VCENTER);
}

pub fn measure_height(hwnd: &w::HWND, face: &w::HFONT, s: &str, width: i32) -> i32 {
    let Ok(dc) = hwnd.GetDC() else { return 0 };
    let Ok(_font) = dc.SelectObject(face) else { return 0 };
    let mut rc = RECT { left: 0, top: 0, right: width, bottom: 0 };
    let _ = dc.DrawText(s, &mut rc, co::DT::CALCRECT | co::DT::WORDBREAK | co::DT::NOPREFIX);
    rc.bottom
}

pub fn buffered(hdc: &w::HDC, rc: RECT, draw: impl FnOnce(&w::HDC)) -> w::SysResult<()> {
    let size = SIZE::with(rc.right - rc.left, rc.bottom - rc.top);
    let mem = hdc.CreateCompatibleDC()?;
    let bitmap = hdc.CreateCompatibleBitmap(size.cx, size.cy)?;
    let _selected = mem.SelectObject(&*bitmap)?;
    draw(&mem);
    hdc.BitBlt(POINT::with(rc.left, rc.top), size, &mem, POINT::with(0, 0), co::ROP::SRCCOPY)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Lamp {
    Off,
    Live,
    Test,
}

pub fn lamp(dc: &w::HDC, rc: RECT, lamp: Lamp, dpi: u32) {
    let (color, rim) = match lamp {
        Lamp::Off => (COLORREF::from_rgb(0x8B, 0x9A, 0x92), COLORREF::from_rgb(0x5E, 0x6D, 0x66)),
        Lamp::Live => (COLORREF::from_rgb(0x35, 0xD2, 0x9A), ACCENT),
        Lamp::Test => (LED_AMBER, COLORREF::from_rgb(0xB9, 0x89, 0x2A)),
    };
    let radius = (rc.right - rc.left) as f32 / 2.0;
    outlined(dc, rc, radius, color, rim, scale(1, dpi), BODY);
}

pub fn lit_segments(db: f32) -> usize {
    (((db - METER_FLOOR_DB) / -METER_FLOOR_DB * METER_SEGMENTS as f32).round().max(0.0) as usize).min(METER_SEGMENTS)
}

fn segment_color(index: usize) -> COLORREF {
    let db = METER_FLOOR_DB - METER_FLOOR_DB * (index as f32 + 0.5) / METER_SEGMENTS as f32;
    if db > -3.0 {
        LED_RED
    } else if db > -12.0 {
        LED_AMBER
    } else {
        LED_GREEN
    }
}

pub struct MeterArt {
    off: DeleteObjectGuard<w::HBITMAP>,
    lit: DeleteObjectGuard<w::HBITMAP>,
    cx: i32,
    cy: i32,
    columns: [i32; METER_SEGMENTS + 1],
}

fn draw_meter(dc: &w::HDC, cx: i32, cy: i32, dpi: u32, number: &w::HFONT, on: bool) -> [i32; METER_SEGMENTS + 1] {
    let rc = RECT { left: 0, top: 0, right: cx, bottom: cy };
    fill(dc, rc, BODY);
    rounded(dc, rc, scale(DISPLAY_CORNER, dpi) as f32, DISPLAY, BODY);

    let inner = inset(rc, scale(12, dpi));
    let inner_w = (inner.right - inner.left) as f32;
    let gap = scale_f(METER_GAP, dpi);
    let width = (inner_w + gap) / METER_SEGMENTS as f32;
    let seg_h = scale(22, dpi);
    let corner = scale_f(2.5, dpi);
    let mut columns = [0; METER_SEGMENTS + 1];
    for (i, column) in columns.iter_mut().enumerate().take(METER_SEGMENTS) {
        let left = inner.left + (width * i as f32).round() as i32;
        let right = inner.left + (width * (i + 1) as f32 - gap).round() as i32;
        let seg = RECT { left, right, top: inner.top, bottom: inner.top + seg_h };
        rounded(dc, seg, corner, if on { segment_color(i) } else { LED_OFF }, DISPLAY);
        *column = left;
    }
    columns[0] = 0;
    columns[METER_SEGMENTS] = cx;

    let half = scale(9, dpi);
    let y = inner.bottom - scale(7, dpi);
    for (i, db) in METER_SCALE_DB.iter().enumerate() {
        let x = inner.left + (inner_w * (*db as f32 - METER_FLOOR_DB) / -METER_FLOOR_DB) as i32;
        let row = RECT { left: inner.left, right: inner.right, top: y - half, bottom: y + half };
        let (cell, align, label) = match i {
            0 => (row, co::DT::LEFT, db.to_string()),
            5 => (row, co::DT::RIGHT, format!("{db} dB")),
            _ => (RECT { left: x - scale(30, dpi), right: x + scale(30, dpi), ..row }, co::DT::CENTER, db.to_string()),
        };
        text(dc, number, cell, &label, DISPLAY_TEXT, align);
    }
    columns
}

impl MeterArt {
    pub fn new(target: &w::HDC, cx: i32, cy: i32, dpi: u32, number: &w::HFONT) -> w::SysResult<Self> {
        let mem = target.CreateCompatibleDC()?;
        let off = target.CreateCompatibleBitmap(cx, cy)?;
        let lit = target.CreateCompatibleBitmap(cx, cy)?;
        let columns = {
            let _selected = mem.SelectObject(&*off)?;
            draw_meter(&mem, cx, cy, dpi, number, false)
        };
        {
            let _selected = mem.SelectObject(&*lit)?;
            draw_meter(&mem, cx, cy, dpi, number, true);
        }
        Ok(Self { off, lit, cx, cy, columns })
    }

    pub fn fits(&self, cx: i32, cy: i32) -> bool {
        (self.cx, self.cy) == (cx, cy)
    }

    pub fn paint(&self, hdc: &w::HDC, lit: usize, peak: usize) -> w::SysResult<()> {
        let on = |i: usize| i < lit || i + 1 == peak;
        let mem = hdc.CreateCompatibleDC()?;
        let mut start = 0;
        for i in 1..=METER_SEGMENTS {
            if i < METER_SEGMENTS && on(i) == on(start) {
                continue;
            }
            let _selected = mem.SelectObject(if on(start) { &*self.lit } else { &*self.off })?;
            let (x0, x1) = (self.columns[start], self.columns[i]);
            hdc.BitBlt(POINT::with(x0, 0), SIZE::with(x1 - x0, self.cy), &mem, POINT::with(x0, 0), co::ROP::SRCCOPY)?;
            start = i;
        }
        Ok(())
    }
}

fn shade(c: COLORREF, f: f32) -> COLORREF {
    let ch = |v: u8| (v as f32 * f).min(255.0) as u8;
    COLORREF::from_rgb(ch(c.GetRValue()), ch(c.GetGValue()), ch(c.GetBValue()))
}

#[derive(Clone, Copy)]
pub struct ButtonState {
    pub pressed: bool,
    pub hovered: bool,
    pub enabled: bool,
    pub focused: bool,
}

pub fn main_button(dc: &w::HDC, rc: RECT, label: &str, running: bool, state: ButtonState, fonts: &Fonts, dpi: u32) {
    let base = if running { SUB_INK } else { ACCENT };
    let color = if !state.enabled {
        EDGE
    } else if state.pressed {
        shade(base, 0.85)
    } else if state.hovered {
        shade(base, 1.15)
    } else {
        base
    };
    fill(dc, rc, BODY);
    let corner = scale(DISPLAY_CORNER, dpi) as f32;
    if state.focused {
        outlined(dc, rc, corner, color, INK, scale(2, dpi), BODY);
    } else {
        rounded(dc, rc, corner, color, BODY);
    }

    let text_w = match dc.SelectObject(&*fonts.main) {
        Ok(_font) => dc.GetTextExtentPoint32(label).map_or(0, |s| s.cx),
        Err(_) => 0,
    };
    let (icon, space) = (scale(12, dpi), scale(8, dpi));
    let left = (rc.left + rc.right - icon - space - text_w) / 2;
    let y = (rc.top + rc.bottom) / 2;
    if running {
        let half = (icon - scale(1, dpi)) / 2;
        let square = RECT { left: left + icon / 2 - half, right: left + icon / 2 + half, top: y - half, bottom: y + half };
        rounded(dc, square, scale_f(1.5, dpi), WHITE, color);
    } else {
        let (x0, x1, half) = ((left + scale(1, dpi)) as f32, (left + icon) as f32, icon as f32 / 2.0);
        let cy = y as f32;
        let norm = half.hypot(x1 - x0);
        let sdf = |x: f32, py: f32| {
            let slant = ((x - x0) * half + (py - cy).abs() * (x1 - x0) - half * (x1 - x0)) / norm;
            slant.max(x0 - x)
        };
        fill_shape(dc, RECT { left, right: left + icon + 1, top: y - icon / 2 - 1, bottom: y + icon / 2 + 1 }, WHITE, color, sdf);
    }
    let cell = RECT { left: left + icon + space, right: rc.right, ..rc };
    text(dc, &fonts.main, cell, label, WHITE, co::DT::LEFT);
}

pub fn push_button(dc: &w::HDC, rc: RECT, label: &str, bg: COLORREF, state: ButtonState, fonts: &Fonts, dpi: u32) {
    let (color, edge) = if !state.enabled {
        (FIELD_DOWN, EDGE)
    } else if state.pressed {
        (FIELD_DOWN, SUB_INK)
    } else if state.hovered {
        (FIELD_HOVER, SUB_INK)
    } else {
        (FIELD, EDGE)
    };
    fill(dc, rc, bg);
    let corner = scale(CORNER, dpi) as f32;
    let line = scale(if state.focused { 2 } else { 1 }, dpi);
    outlined(dc, rc, corner, color, if state.focused { INK } else { edge }, line, bg);
    let ink = if state.enabled { INK } else { SUB_INK };
    text(dc, &fonts.body, rc, label, ink, co::DT::CENTER);
}

pub fn disclosure(dc: &w::HDC, rc: RECT, label: &str, open: bool, state: ButtonState, fonts: &Fonts, dpi: u32) {
    fill(dc, rc, BODY);
    if state.focused {
        outlined(dc, rc, scale(4, dpi) as f32, BODY, INK, scale(1, dpi), BODY);
    }
    let color = if state.hovered { INK } else { SUB_INK };
    let half = scale_f(4.0, dpi);
    let (cx, cy) = (rc.left as f32 + scale(10, dpi) as f32, (rc.top + rc.bottom) as f32 / 2.0);
    let long = half.hypot(half * 1.6);
    let sdf = |x: f32, y: f32| {
        let (across, along) = if open { (x - cx, y - cy) } else { (y - cy, x - cx) };
        let slant = (across.abs() * half * 1.6 + along * half - half * half * 1.6) / long;
        slant.max(-along - half)
    };
    let (bx, by, box_half) = (cx as i32, cy as i32, scale(8, dpi));
    fill_shape(dc, RECT { left: bx - box_half, right: bx + box_half, top: by - box_half, bottom: by + box_half }, color, BODY, sdf);
    let cell = RECT { left: rc.left + scale(22, dpi), ..rc };
    text(dc, &fonts.body, cell, label, INK, co::DT::LEFT);
}

#[derive(Clone, Copy)]
pub enum Tone {
    Danger,
    Caution,
}

impl Tone {
    pub fn fill(self) -> COLORREF {
        match self {
            Tone::Danger => COLORREF::from_rgb(0xF7, 0xD9, 0xD5),
            Tone::Caution => COLORREF::from_rgb(0xF6, 0xE6, 0xBF),
        }
    }

    pub fn edge(self) -> COLORREF {
        match self {
            Tone::Danger => COLORREF::from_rgb(0xC9, 0x48, 0x3C),
            Tone::Caution => COLORREF::from_rgb(0xB9, 0x89, 0x2A),
        }
    }

    pub fn ink(self) -> COLORREF {
        match self {
            Tone::Danger => COLORREF::from_rgb(0x7A, 0x22, 0x19),
            Tone::Caution => COLORREF::from_rgb(0x5E, 0x43, 0x0C),
        }
    }
}

pub fn banner(dc: &w::HDC, rc: RECT, tone: Tone, dpi: u32) {
    outlined(dc, rc, scale(CORNER, dpi) as f32, tone.fill(), tone.edge(), scale(1, dpi), BODY);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meter_maps_decibels_to_segments() {
        assert_eq!(lit_segments(-200.0), 0);
        assert_eq!(lit_segments(-60.0), 0);
        assert_eq!(lit_segments(-30.0), METER_SEGMENTS / 2);
        assert_eq!(lit_segments(0.0), METER_SEGMENTS);
        assert_eq!(lit_segments(6.0), METER_SEGMENTS);
    }

    #[test]
    fn meter_zones_rise_towards_clipping() {
        assert!(segment_color(0) == LED_GREEN);
        assert!(segment_color(lit_segments(-8.0)) == LED_AMBER);
        assert!(segment_color(METER_SEGMENTS - 1) == LED_RED);
    }
}
