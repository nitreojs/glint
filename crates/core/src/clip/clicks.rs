use serde::{Deserialize, Serialize};

use super::crop::Crop;
use super::math::Range;
use crate::record::clicks::{Button, Click, Payload, Shape, Style};

pub const TAP: i64 = 800_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClickSize {
    Small,
    #[default]
    Normal,
    Large,
}

impl ClickSize {
    pub fn factor(self) -> f32 {
        match self {
            ClickSize::Small => 0.7,
            ClickSize::Normal => 1.0,
            ClickSize::Large => 1.5,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClickColor {
    #[default]
    Accent,
    White,
    Custom([u8; 3]),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClickLook {
    pub size: ClickSize,
    pub color: ClickColor,
}

impl ClickLook {
    pub fn style(&self, accent: [u8; 3], scale: f32) -> Style {
        let rgb = match self.color {
            ClickColor::Accent => accent,
            ClickColor::White => [255, 255, 255],
            ClickColor::Custom(c) => c,
        };
        Style::new(rgb, scale * self.size.factor())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Clicks {
    pub list: Vec<Click>,
    pub shown: bool,
    pub scale: f32,
}

impl Clicks {
    pub fn new(payload: &Payload) -> Clicks {
        Clicks { list: payload.clicks.clone(), shown: true, scale: payload.scale }
    }

    pub fn shift(&mut self, picked: &[usize], by: i64, duration: i64) -> bool {
        let downs = || picked.iter().filter_map(|&i| self.list.get(i)).map(|c| c.down);
        let (Some(lo), Some(hi)) = (downs().min(), downs().max()) else { return false };
        let by = by.clamp(-lo, (duration - 1 - hi).max(-lo));
        if by == 0 {
            return false;
        }
        for &i in picked {
            if let Some(c) = self.list.get_mut(i) {
                c.down += by;
                c.up = c.up.map(|u| u + by);
            }
        }
        true
    }

    pub fn delete(&mut self, picked: &[usize]) -> bool {
        let before = self.list.len();
        let mut i = 0;
        self.list.retain(|_| {
            i += 1;
            !picked.contains(&(i - 1))
        });
        self.list.len() != before
    }

    pub fn set_button(&mut self, picked: &[usize], button: Button) -> bool {
        let mut changed = false;
        for &i in picked {
            if let Some(c) = self.list.get_mut(i).filter(|c| c.button != button) {
                c.button = button;
                changed = true;
            }
        }
        changed
    }

    pub fn place(&mut self, i: usize, x: f32, y: f32, size: (u32, u32)) -> bool {
        let Some(c) = self.list.get_mut(i) else { return false };
        let (x, y) = (x.clamp(0.0, size.0 as f32), y.clamp(0.0, size.1 as f32));
        let changed = (c.x, c.y) != (x, y);
        (c.x, c.y) = (x, y);
        changed
    }

    pub fn add(&mut self, t: i64, x: f32, y: f32) -> usize {
        self.list.push(Click { button: Button::Left, down: t, up: Some(t + TAP), x, y });
        self.list.len() - 1
    }

    pub fn between(&self, a: i64, b: i64) -> Vec<usize> {
        let (lo, hi) = (a.min(b), a.max(b));
        (0..self.list.len()).filter(|&i| (lo..=hi).contains(&self.list[i].down)).collect()
    }

    pub fn burnt(&self, kept: &[Range], style: &Style) -> Vec<Click> {
        if !self.shown {
            return Vec::new();
        }
        let show = |c: &Click| kept.iter().any(|r| visible(c, style).0 < r.end && visible(c, style).1 > r.start);
        self.list.iter().copied().filter(show).collect()
    }
}

pub fn visible(c: &Click, style: &Style) -> (i64, i64) {
    let end = match c.up {
        Some(u) => (c.down + style.ripple).max(u.max(c.down) + style.fade),
        None => i64::MAX,
    };
    (c.down, end)
}

pub fn into_output(shapes: &mut [Shape], crop: Crop, out: (u32, u32)) {
    let k = out.0 as f32 / crop.w.max(1) as f32;
    for s in shapes {
        s.x = (s.x - crop.x as f32) * k;
        s.y = (s.y - crop.y as f32) * out.1 as f32 / crop.h.max(1) as f32;
        s.radius *= k;
        s.width *= k;
    }
}

pub fn paint_bgrx(px: &mut [u8], pitch: usize, size: (u32, u32), shapes: &[Shape]) {
    for s in shapes {
        let reach = s.radius + s.width * 0.5 + 1.0;
        let (x0, x1) = (((s.x - reach).floor().max(0.0)) as u32, ((s.x + reach).ceil().max(0.0) as u32).min(size.0));
        let (y0, y1) = (((s.y - reach).floor().max(0.0)) as u32, ((s.y + reach).ceil().max(0.0) as u32).min(size.1));
        let [r, g, b, alpha] = s.color;
        for y in y0..y1 {
            let row = y as usize * pitch;
            for x in x0..x1 {
                let d = ((x as f32 + 0.5 - s.x).powi(2) + (y as f32 + 0.5 - s.y).powi(2)).sqrt();
                let a = alpha * (s.width * 0.5 - (d - s.radius).abs() + 0.5).clamp(0.0, 1.0);
                if a <= 0.0 {
                    continue;
                }
                let p = &mut px[row + x as usize * 4..row + x as usize * 4 + 3];
                for (v, c) in p.iter_mut().zip([b, g, r]) {
                    *v = (*v as f32 * (1.0 - a) + c * 255.0 * a).round() as u8;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clip::edit::{Edit, Undo};
    use crate::clip::math::SECOND;
    use crate::record::clicks::state_at;

    const S: i64 = SECOND;

    fn click(button: Button, down: i64, up: Option<i64>) -> Click {
        Click { button, down, up, x: 100.5, y: 50.5 }
    }

    fn clicks() -> Clicks {
        let list = vec![click(Button::Left, S, Some(S + S / 10)), click(Button::Right, 3 * S, Some(5 * S)), click(Button::Middle, 8 * S, None)];
        Clicks::new(&Payload { version: 1, scale: 1.5, clicks: list })
    }

    #[test]
    fn clicks_move_delete_change_and_get_added_with_undo() {
        let mut e = Edit::new(10 * S, 0);
        e.clicks = Some(clicks());
        let fresh = e.clone();
        let mut u = Undo::default();
        let mut step = |e: &mut Edit, f: &dyn Fn(&mut Clicks) -> bool| {
            let before = e.clone();
            let changed = f(e.clicks.as_mut().unwrap());
            u.record(before, e);
            changed
        };

        assert!(step(&mut e, &|c| c.shift(&[0, 1], S / 2, 10 * S)));
        let c = &e.clicks.as_ref().unwrap().list;
        assert_eq!((c[0].down, c[1].down, c[1].up), (S + S / 2, 3 * S + S / 2, Some(5 * S + S / 2)));
        assert!(step(&mut e, &|c| c.shift(&[2], 5 * S, 10 * S)), "stops at the end");
        assert_eq!(e.clicks.as_ref().unwrap().list[2].down, 10 * S - 1);
        assert!(step(&mut e, &|c| c.shift(&[0, 2], -20 * S, 10 * S)), "stops at the start for all of them");
        assert_eq!(e.clicks.as_ref().unwrap().list[0].down, 0);
        assert!(!step(&mut e, &|c| c.shift(&[0], -S, 10 * S)));
        assert!(!step(&mut e, &|c| c.shift(&[7], S, 10 * S)));

        assert!(step(&mut e, &|c| c.set_button(&[0, 1], Button::Middle)));
        assert!(!step(&mut e, &|c| c.set_button(&[0], Button::Middle)));
        assert!(step(&mut e, &|c| c.place(1, -5.0, 9000.0, (640, 360))));
        assert_eq!((e.clicks.as_ref().unwrap().list[1].x, e.clicks.as_ref().unwrap().list[1].y), (0.0, 360.0));
        assert!(step(&mut e, &|c| c.delete(&[0, 2])));
        assert_eq!(e.clicks.as_ref().unwrap().list.len(), 1);
        assert!(!step(&mut e, &|c| c.delete(&[5])));
        assert!(step(&mut e, &|c| c.add(2 * S, 10.0, 20.0) == 1));
        assert_eq!(e.clicks.as_ref().unwrap().list[1], Click { button: Button::Left, down: 2 * S, up: Some(2 * S + TAP), x: 10.0, y: 20.0 });
        assert_eq!(e.clicks.as_ref().unwrap().between(3 * S, 0), [1]);

        while u.undo(&mut e) {}
        assert_eq!(e, fresh);
    }

    #[test]
    fn the_export_draws_only_shown_clicks_that_show_in_the_kept_ranges() {
        let style = ClickLook::default().style([200, 30, 30], 1.0);
        let mut c = clicks();
        let r = |a: i64, b: i64| Range { start: a, end: b };
        assert_eq!(c.burnt(&[r(0, 10 * S)], &style).len(), 3);
        let downs = |v: Vec<Click>| v.iter().map(|c| c.down).collect::<Vec<_>>();
        assert_eq!(downs(c.burnt(&[r(2 * S, 7 * S)], &style)), [3 * S], "trimmed off on both sides");
        assert_eq!(downs(c.burnt(&[r(S + S / 5, 2 * S)], &style)), [S], "a ripple still fading after the cut start");
        assert_eq!(downs(c.burnt(&[r(S + S / 2, 2 * S)], &style)), Vec::<i64>::new(), "long gone by then");
        assert_eq!(downs(c.burnt(&[r(0, S / 2), r(5 * S + S / 10, 6 * S)], &style)), [3 * S], "a held dot fading into a kept piece");
        assert_eq!(downs(c.burnt(&[r(9 * S, 10 * S)], &style)), [8 * S], "held to the end");
        c.shown = false;
        assert!(c.burnt(&[r(0, 10 * S)], &style).is_empty());
        assert_eq!(visible(&c.list[0], &style), (S, S + style.ripple));
        assert_eq!(visible(&c.list[1], &style), (3 * S, 5 * S + style.fade));
    }

    #[test]
    fn looks_scale_and_colour_the_style() {
        let small = ClickLook { size: ClickSize::Small, color: ClickColor::White }.style([1, 2, 3], 2.0);
        assert!((small.radius - 28.0 * 1.4).abs() < 1e-4);
        assert_eq!(small.left, [1.0, 1.0, 1.0, 1.0]);
        let custom = ClickLook { size: ClickSize::Large, color: ClickColor::Custom([255, 0, 0]) }.style([1, 2, 3], 1.0);
        assert_eq!((custom.radius, custom.left), (42.0, [1.0, 0.0, 0.0, 1.0]));
        assert_eq!(ClickLook::default().style([255, 0, 255], 1.0).left, [1.0, 0.0, 1.0, 1.0]);
        let json = serde_json::to_string(&ClickLook { size: ClickSize::Large, color: ClickColor::Custom([1, 2, 3]) }).unwrap();
        assert_eq!(json, r#"{"size":"large","color":{"custom":[1,2,3]}}"#);
        assert_eq!(serde_json::from_str::<ClickLook>("{}").unwrap(), ClickLook::default());
    }

    #[test]
    fn shapes_follow_the_crop_and_scale_and_paint_like_the_shader() {
        let mut shapes = vec![Shape { x: 300.0, y: 200.0, radius: 20.0, width: 4.0, color: [1.0, 0.0, 0.0, 1.0] }];
        into_output(&mut shapes, Crop { x: 100, y: 100, w: 400, h: 200 }, (200, 100));
        assert_eq!(shapes[0], Shape { x: 100.0, y: 50.0, radius: 10.0, width: 2.0, color: [1.0, 0.0, 0.0, 1.0] });

        let (w, h) = (64u32, 48u32);
        let pitch = w as usize * 4 + 16;
        let mut px = vec![40u8; pitch * h as usize];
        let ring = Shape { x: 32.0, y: 24.0, radius: 10.0, width: 3.0, color: [1.0, 0.0, 0.0, 1.0] };
        paint_bgrx(&mut px, pitch, (w, h), &[ring]);
        let at = |x: usize, y: usize| &px[y * pitch + x * 4..y * pitch + x * 4 + 4];
        assert_eq!(at(41, 24), [0, 0, 255, 40], "on the ring: full colour, the fourth byte kept");
        assert_eq!(at(32, 24), [40, 40, 40, 40], "the middle stays");
        assert_eq!(at(5, 5), [40, 40, 40, 40]);
        let edge = at(43, 24)[2];
        assert!(edge > 40 && edge < 255, "antialiased edge: {edge}");
        assert!(px[w as usize * 4..pitch].iter().all(|&b| b == 40), "the row padding is never touched");

        let style = Style::new([0, 255, 0], 1.0);
        let dot = state_at(&[Click { button: Button::Left, down: 0, up: None, x: 10.5, y: 10.5 }], 0, &style);
        let mut px = vec![0u8; pitch * h as usize];
        paint_bgrx(&mut px, pitch, (w, h), &dot);
        assert_eq!(&px[10 * pitch + 40..10 * pitch + 44], [0, 255, 0, 0], "a held dot fills its centre");
        let clipped = Shape { x: -5.0, y: 47.0, radius: 30.0, width: 6.0, color: [1.0; 4] };
        paint_bgrx(&mut px, pitch, (w, h), &[clipped]);
    }
}
