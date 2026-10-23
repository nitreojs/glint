use std::sync::LazyLock;

use eframe::egui::{Color32, Context, Id, Rect, Ui, UiBuilder, pos2, vec2};
use windows::Win32::UI::WindowsAndMessaging::{
    SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
};
use windows::core::BOOL;

/// Windows' "show animations in Windows" setting; when it's off every transition snaps.
static MOTION: LazyLock<bool> = LazyLock::new(|| {
    let mut on = BOOL(1);
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some(&mut on as *mut BOOL as _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    ok.is_err() || on.as_bool()
});

/// Whether transitions animate at all.
pub fn motion() -> bool {
    *MOTION
}

/// Linear 0..=1 progress toward `on`, over `secs`.
pub fn anim(ui: &Ui, id: Id, on: bool, secs: f32) -> f32 {
    anim_ctx(ui.ctx(), id, on, secs)
}

pub fn anim_ctx(ctx: &Context, id: Id, on: bool, secs: f32) -> f32 {
    if *MOTION {
        ctx.animate_bool_with_time(id, on, secs)
    } else if on {
        1.0
    } else {
        0.0
    }
}

/// The design's ease-out, cubic-bezier(0.2, 0, 0, 1).
pub fn ease(t: f32) -> f32 {
    if t <= 0.0 || t >= 1.0 {
        return t.clamp(0.0, 1.0);
    }
    let bez = |s: f32, a: f32, b: f32| 3.0 * (1.0 - s).powi(2) * s * a + 3.0 * (1.0 - s) * s * s * b + s * s * s;
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..20 {
        let mid = (lo + hi) / 2.0;
        if bez(mid, 0.2, 0.0) < t { lo = mid } else { hi = mid }
    }
    bez((lo + hi) / 2.0, 0.0, 1.0)
}

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    a.lerp_to_gamma(b, t.clamp(0.0, 1.0))
}

#[derive(Clone, Copy)]
struct Glide {
    from: f32,
    to: f32,
    start: f64,
}

/// `target`, reached with the design's ease-out over `secs`; when the target changes mid-way
/// the motion restarts from wherever it is, so it never jumps. `secs` 0 follows exactly.
pub fn glide(ui: &Ui, id: Id, target: f32, secs: f32) -> f32 {
    let now = ui.input(|i| i.time);
    let ctx = ui.ctx();
    let progress = |g: &Glide| if *MOTION && secs > 0.0 { ((now - g.start) as f32 / secs).clamp(0.0, 1.0) } else { 1.0 };
    let value = |g: &Glide| g.from + (g.to - g.from) * ease(progress(g));
    let mut g = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(id, || Glide { from: target, to: target, start: now }));
    if g.to != target {
        g = Glide { from: value(&g), to: target, start: now };
        ctx.data_mut(|d| d.insert_temp(id, g));
    }
    if progress(&g) < 1.0 {
        ctx.request_repaint();
    }
    value(&g)
}

/// Sets the motion at `id` to `value` with nothing in flight, so the next [`glide`] or
/// [`reveal`] moves away from it: what was just added starts folded and opens.
pub fn snap(ctx: &Context, id: Id, value: f32) {
    let now = ctx.input(|i| i.time);
    ctx.data_mut(|d| d.insert_temp(id, Glide { from: value, to: value, start: now }));
}

/// How long content takes to open or fold away.
const REVEAL: f32 = 0.18;
/// How far opening content drops in from.
const DROP: f32 = 6.0;

/// Content that opens where it stands while `on` and folds away when it turns off: its height, a
/// short drop and its opacity follow one eased curve, and whatever comes after moves along with it.
/// `None` once it's folded.
pub fn reveal<R>(ui: &mut Ui, id: Id, on: bool, add: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    let t = glide(ui, id, if on { 1.0 } else { 0.0 }, REVEAL);
    if t <= 0.0 && !on {
        // the child and the space below take two automatic ids; keep later widgets' ids stable
        ui.skip_ahead_auto_ids(2);
        return None;
    }
    let height_id = id.with("height");
    let known = ui.data(|d| d.get_temp::<f32>(height_id));
    // The gap left after the last item opens with the content, so nothing jumps by it; whatever
    // spacing the content leaves behind opens with it too.
    let spacing = ui.spacing().item_spacing.y;
    let cursor = ui.cursor().top();
    let gap = (cursor - ui.min_rect().bottom()).max(0.0);
    let top = cursor - gap + t * gap;
    let avail = ui.available_rect_before_wrap();
    let drop = (1.0 - t) * DROP;
    let max = Rect::from_min_max(pos2(avail.left(), top - drop), pos2(avail.right(), avail.bottom().max(top)));
    let mut child = ui.new_child(UiBuilder::new().scope_id(id.with("body")).max_rect(max).layout(*ui.layout()));
    if t < 1.0 {
        // last frame's height; a first frame (t is still 0 then) shows nothing
        let shown = known.unwrap_or(0.0) * t;
        child.shrink_clip_rect(Rect::from_x_y_ranges(ui.clip_rect().x_range(), top..=top + shown));
        child.multiply_opacity(t);
    }
    let out = add(&mut child);
    let content = child.min_rect();
    let height = (content.bottom() - max.top()).max(0.0);
    if known != Some(height) {
        ui.data_mut(|d| d.insert_temp(height_id, height));
        ui.ctx().request_repaint();
    }
    ui.advance_cursor_after_rect(Rect::from_min_size(pos2(avail.left(), top), vec2(content.width(), t * height)));
    ui.add_space((1.0 - t) * (gap - spacing));
    Some(out)
}

/// [`reveal`] for content drawn from a value that's already gone when it should fold, like an
/// error that was cleared: the last value stays on screen while it folds away.
pub fn reveal_with<T: Clone + Send + Sync + 'static, R>(
    ui: &mut Ui,
    id: Id,
    value: Option<T>,
    add: impl FnOnce(&mut Ui, T) -> R,
) -> Option<R> {
    let last_id = id.with("last");
    let on = value.is_some();
    let value = match value {
        Some(v) => {
            ui.data_mut(|d| d.insert_temp(last_id, v.clone()));
            Some(v)
        }
        None => ui.data(|d| d.get_temp::<T>(last_id)),
    };
    match value {
        Some(v) => reveal(ui, id, on, |ui| add(ui, v)),
        None => reveal(ui, id, false, |_| None).flatten(),
    }
}

/// A list as it's drawn with [`reveal`]: the current items in order, plus removed ones left at
/// their old place, drawn from the copy kept here, until they've folded away. Items that show up
/// after the first frame start folded, so they open.
pub struct Listed<K, T> {
    drawn: Vec<(K, T)>,
    started: bool,
}

impl<K, T> Default for Listed<K, T> {
    fn default() -> Self {
        Listed { drawn: Vec::new(), started: false }
    }
}

impl<K: Clone + PartialEq, T: Clone> Listed<K, T> {
    /// Takes the list as it is now; returns what to draw, each with whether it's still in the list.
    /// `id` is the motion id [`reveal`] gets for an item.
    pub fn update(&mut self, ctx: &Context, id: impl Fn(&K) -> Id, current: Vec<(K, T)>) -> Vec<(K, T, bool)> {
        let mut out: Vec<(K, T, bool)> = current.into_iter().map(|(k, t)| (k, t, true)).collect();
        for (pos, (key, item)) in self.drawn.iter().enumerate() {
            if out.iter().any(|(k, ..)| k == key) {
                continue;
            }
            // right after whichever item it followed before
            let at = self.drawn[..pos]
                .iter()
                .rev()
                .find_map(|(before, _)| out.iter().position(|(k, ..)| k == before))
                .map_or(0, |i| i + 1);
            out.insert(at, (key.clone(), item.clone(), false));
        }
        if self.started {
            for (key, _, present) in &out {
                if *present && !self.drawn.iter().any(|(k, _)| k == key) {
                    snap(ctx, id(key), 0.0);
                }
            }
        }
        self.started = true;
        self.drawn = out.iter().map(|(k, t, _)| (k.clone(), t.clone())).collect();
        out
    }

    /// Lets go of a removed item once [`reveal`] reports it folded.
    pub fn folded(&mut self, key: &K) {
        self.drawn.retain(|(k, _)| k != key);
    }
}
