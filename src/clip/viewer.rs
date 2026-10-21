use eframe::egui::{Vec2, vec2};

/// Physical pixels per video pixel the viewer allows, 10 % to 1600 %.
pub(super) const MIN_ZOOM: f32 = 0.1;
pub(super) const MAX_ZOOM: f32 = 16.0;
const SECS: f32 = 0.18;

/// Where the frame sits in the view: points per video pixel and the frame's top-left relative to the view's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Place {
    pub scale: f32,
    pub offset: Vec2,
}

pub(super) fn fit(img: Vec2, view: Vec2) -> Place {
    let scale = (view.x / img.x.max(1.0)).min(view.y / img.y.max(1.0));
    Place { scale, offset: (view - img * scale) / 2.0 }
}

/// A frame smaller than the view stays centred on that axis; a bigger one can't be pulled past its edges.
pub(super) fn clamp(p: Place, img: Vec2, view: Vec2) -> Place {
    let size = img * p.scale;
    let axis = |off: f32, size: f32, view: f32| if size <= view { (view - size) / 2.0 } else { off.clamp(view - size, 0.0) };
    Place { scale: p.scale, offset: vec2(axis(p.offset.x, size.x, view.x), axis(p.offset.y, size.y, view.y)) }
}

/// `scale`, keeping the video pixel under `at` (view coordinates) where it is.
pub(super) fn zoom_at(p: Place, at: Vec2, scale: f32, img: Vec2, view: Vec2) -> Place {
    let under = (at - p.offset) / p.scale;
    clamp(Place { scale, offset: at - under * scale }, img, view)
}

pub(super) fn pan(p: Place, by: Vec2, img: Vec2, view: Vec2) -> Place {
    clamp(Place { offset: p.offset + by, ..p }, img, view)
}

/// Halfway along a zoom: the scale moves evenly in log space while the anchor's video pixel stays put, and
/// whatever clamping the end needs blends in.
pub(super) fn between(from: Place, to: Place, anchor: Vec2, e: f32) -> Place {
    let under = (anchor - from.offset) / from.scale;
    let scale = (from.scale.ln() + (to.scale.ln() - from.scale.ln()) * e).exp();
    let free_end = anchor - under * to.scale;
    Place { scale, offset: anchor - under * scale + (to.offset - free_end) * e }
}

pub(super) struct Viewer {
    pub fitted: bool,
    from: Place,
    to: Place,
    anchor: Vec2,
    start: f64,
    view: Vec2,
    pub changed_at: f64,
}

impl Viewer {
    pub(super) fn new() -> Viewer {
        let p = Place { scale: 1.0, offset: Vec2::ZERO };
        Viewer { fitted: true, from: p, to: p, anchor: Vec2::ZERO, start: f64::NEG_INFINITY, view: Vec2::ZERO, changed_at: f64::NEG_INFINITY }
    }

    fn progress(&self, now: f64, motion: bool) -> f32 {
        if motion { ((now - self.start) as f32 / SECS).clamp(0.0, 1.0) } else { 1.0 }
    }

    pub(super) fn animating(&self, now: f64, motion: bool) -> bool {
        self.progress(now, motion) < 1.0
    }

    /// The placement to draw this frame; a fitted view follows the view's size.
    pub(super) fn place(&mut self, now: f64, img: Vec2, view: Vec2, motion: bool, ease: impl Fn(f32) -> f32) -> Place {
        if self.view != view {
            self.view = view;
            if self.fitted && !self.animating(now, motion) {
                self.to = fit(img, view);
                self.from = self.to;
            } else if !self.fitted {
                self.to = clamp(self.to, img, view);
            }
        }
        if self.fitted && !self.animating(now, motion) {
            self.to = fit(img, view);
        }
        let t = self.progress(now, motion);
        if t >= 1.0 { self.to } else { between(self.from, self.to, self.anchor, ease(t)) }
    }

    fn go(&mut self, now: f64, current: Place, to: Place, anchor: Vec2) {
        self.from = current;
        self.to = to;
        self.anchor = anchor;
        self.start = now;
        self.changed_at = now;
    }

    pub(super) fn target(&self) -> Place {
        self.to
    }

    /// Multiplies the target zoom around `at`, within 10 % to 1600 % of the screen's pixels.
    pub(super) fn zoom(&mut self, now: f64, current: Place, at: Vec2, factor: f32, ppp: f32, img: Vec2) {
        let scale = (self.to.scale * factor).clamp(MIN_ZOOM / ppp, MAX_ZOOM / ppp);
        let to = zoom_at(self.to, at, scale, img, self.view);
        self.fitted = false;
        self.go(now, current, to, at);
    }

    pub(super) fn actual_size(&mut self, now: f64, current: Place, ppp: f32, img: Vec2) {
        let at = self.view / 2.0;
        let to = zoom_at(current, at, 1.0 / ppp, img, self.view);
        self.fitted = false;
        self.go(now, current, to, at);
    }

    pub(super) fn fit_view(&mut self, now: f64, current: Place, img: Vec2) {
        let to = fit(img, self.view);
        self.fitted = true;
        self.go(now, current, to, self.view / 2.0);
    }

    pub(super) fn pan(&mut self, by: Vec2, current: Place, img: Vec2) {
        let p = pan(current, by, img, self.view);
        self.fitted = false;
        self.from = p;
        self.to = p;
        self.start = f64::NEG_INFINITY;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMG: Vec2 = vec2(1920.0, 1080.0);
    const VIEW: Vec2 = vec2(960.0, 600.0);

    fn near(a: Vec2, b: Vec2) -> bool {
        (a - b).length() < 1e-3
    }

    #[test]
    fn fitting_centres_the_frame() {
        let p = fit(IMG, VIEW);
        assert_eq!(p.scale, 0.5);
        assert_eq!(p.offset, vec2(0.0, 30.0));
    }

    #[test]
    fn zooming_keeps_the_pixel_under_the_cursor() {
        let p = fit(IMG, VIEW);
        let at = vec2(300.0, 200.0);
        let under = (at - p.offset) / p.scale;
        let z = zoom_at(p, at, 2.0, IMG, VIEW);
        assert!(near(z.offset + under * z.scale, at));
        for e in [0.0, 0.3, 0.7, 1.0] {
            let mid = between(p, z, at, e);
            assert!(near(mid.offset + under * mid.scale, at), "{e}");
        }
        assert!((between(p, z, at, 0.5).scale - 1.0).abs() < 1e-4);
        assert_eq!(between(p, z, at, 1.0), z);
        assert_eq!(between(p, z, at, 0.0), p);
    }

    #[test]
    fn panning_never_shows_past_the_frame_edges() {
        let big = Place { scale: 2.0, offset: vec2(-100.0, -100.0) };
        assert_eq!(pan(big, vec2(500.0, 500.0), IMG, VIEW).offset, vec2(0.0, 0.0));
        assert_eq!(pan(big, vec2(-9000.0, -9000.0), IMG, VIEW).offset, vec2(960.0 - 3840.0, 600.0 - 2160.0));
        let small = Place { scale: 0.25, offset: vec2(0.0, 0.0) };
        assert_eq!(pan(small, vec2(77.0, -40.0), IMG, VIEW).offset, vec2(240.0, 165.0));
        let wide = Place { scale: 0.75, offset: vec2(0.0, 0.0) };
        assert_eq!(pan(wide, vec2(-50.0, 50.0), IMG, VIEW).offset, vec2(-50.0, 0.0));
        let edge = zoom_at(fit(IMG, VIEW), vec2(0.0, 0.0), 1.0, IMG, VIEW);
        assert_eq!(edge.offset, vec2(0.0, 0.0));
    }
}
