use std::sync::LazyLock;

use eframe::egui::{Color32, Context, Id, Ui};
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
