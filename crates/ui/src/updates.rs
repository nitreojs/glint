//! Updates in a settings window, for any of our apps: the tray app's status (its status.json,
//! re-read whenever it changes), the "updates" rows, and the update button that rises at the
//! bottom of the sidebar while there's an update to take. The settings window is a process of
//! its own: its buttons ask the tray app, which does the work and reports back through the file.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use eframe::egui::{
    Color32, CursorIcon, Id, Mesh, Painter, Rect, RichText, Sense, Shape, Ui, WidgetInfo, accesskit::Role, pos2,
    vec2,
};
use glint_core::config::UpdateMode;
use glint_core::update::status::{State, Status};
use glint_core::update::{RELEASES_PAGE, notes_url};
use glint_shell::update::{Build, CHECK_BETA, CHECK_DOWNLOAD, WM_UPDATE_CHECK, WM_UPDATE_INSTALL};

use crate::gallery::open_url;
use crate::widgets::{self, Kind, Look, ease, focus_ring, hint, medium, mix, motion, row, sans, toggle_row};

/// The tray app's update status as the settings window knows it.
pub struct Updater {
    build: Build,
    /// The tray app's main window class, which the requests go to.
    main_class: &'static str,
    path: PathBuf,
    stamp: Option<SystemTime>,
    pub status: Status,
    /// Why the last request went nowhere.
    pub note: Option<String>,
}

impl Updater {
    pub fn new(build: Build, main_class: &'static str) -> Updater {
        Updater::at(build, main_class, build.status_path())
    }

    /// An updater reading the status from `path`.
    pub fn at(build: Build, main_class: &'static str, path: PathBuf) -> Updater {
        let mut u = Updater { build, main_class, path, stamp: None, status: Status::default(), note: None };
        u.poll();
        u
    }

    /// Re-reads the status file when it changed.
    pub fn poll(&mut self) {
        let stamp = std::fs::metadata(&self.path).and_then(|m| m.modified()).ok();
        if stamp != self.stamp {
            self.stamp = stamp;
            self.status = Status::load_from(&self.path);
        }
    }

    /// Sends `msg` to the tray app; `meanwhile` shows until it reports back.
    fn ask(&mut self, msg: u32, wparam: usize, meanwhile: State) {
        if glint_shell::window::post(self.main_class, msg, wparam) {
            self.status.state = meanwhile;
            self.note = None;
        } else {
            self.note = Some(format!("{} isn't running in the tray; start it first", self.build.product.name));
        }
    }

    /// Checks now, downloading what's found when `mode` is auto.
    pub fn check_now(&mut self, mode: UpdateMode, beta: bool) {
        let download = if mode == UpdateMode::Auto { CHECK_DOWNLOAD } else { 0 };
        let beta = if beta { CHECK_BETA } else { 0 };
        self.ask(WM_UPDATE_CHECK, download | beta, State::Checking);
    }

    /// Restarts into the update, downloading it first when it's only known.
    pub fn install(&mut self) {
        let meanwhile = match &self.status.state {
            State::Available { version } => State::Downloading { version: version.clone(), percent: 0 },
            state => state.clone(),
        };
        self.ask(WM_UPDATE_INSTALL, 0, meanwhile);
    }

    fn notes(&self, version: &str) -> String {
        notes_url(&format!("{}{version}", self.build.product.tag_prefix))
    }
}

/// The updates rows: the mode, the beta channel, the status with "check now", release notes.
pub fn section(ui: &mut Ui, look: &Look, updater: &mut Updater, mode: &mut UpdateMode, beta: &mut bool) {
    let p = look.p;
    let name = updater.build.product.name;
    updater.poll();
    ui.ctx().request_repaint_after(Duration::from_millis(500));
    row(ui, look, "updates", 7.0, |ui| {
        let modes = [(UpdateMode::Auto, "auto"), (UpdateMode::Notify, "notify only"), (UpdateMode::Off, "off")];
        widgets::segmented(ui, look, mode, &modes, Some(300.0));
        let about = match mode {
            UpdateMode::Auto => format!("downloads in the background, installs when {name} restarts"),
            UpdateMode::Notify => "a notification asks first".into(),
            UpdateMode::Off => "never checks on its own".into(),
        };
        hint(ui, about, p.t3);
    });
    toggle_row(ui, look, "beta updates", beta, "also take pre-releases");
    row(ui, look, "status", 7.0, |ui| status(ui, look, updater, *mode, *beta));
    row(ui, look, "release notes", 0.0, |ui| {
        let (text, url) = match updater.status.state.version() {
            Some(version) => (format!("what's new in {version} ↗"), updater.notes(version)),
            None => ("github.com/nitreojs/glint/releases ↗".to_string(), RELEASES_PAGE.to_string()),
        };
        if widgets::link(ui, look, &text).clicked() {
            open_url(&url);
        }
    });
}

fn status(ui: &mut Ui, look: &Look, updater: &mut Updater, mode: UpdateMode, beta: bool) {
    let p = look.p;
    let state = updater.status.state.clone();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        if widgets::button(ui, look, "check now", Kind::Secondary, 32.0, !state.busy()).clicked() {
            updater.check_now(mode, beta);
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            let text = |ui: &mut Ui, text: &str| ui.label(RichText::new(text).font(sans(13.0)).color(p.t2));
            match &state {
                State::Unknown | State::Failed { .. } => {}
                State::Checking => {
                    text(ui, "checking…");
                }
                State::UpToDate => {
                    text(ui, "up to date");
                }
                State::Downloading { version, percent } => {
                    text(ui, &format!("downloading {version}… {percent}%"));
                }
                State::Available { version } => {
                    text(ui, &format!("{version} available ·"));
                    if widgets::link(ui, look, "install").clicked() {
                        updater.install();
                    }
                }
                State::Ready { version } => {
                    text(ui, &format!("{version} ready ·"));
                    if widgets::link(ui, look, "restart").clicked() {
                        updater.install();
                    }
                }
            }
        });
    });
    let failed = match &state {
        State::Failed { error } => Some(error.clone()),
        _ => None,
    };
    widgets::reveal_with(ui, Id::unique("update-failed"), failed, |ui, error| hint(ui, error, p.warn));
    widgets::reveal_with(ui, Id::unique("update-note"), updater.note.clone(), |ui, note| hint(ui, note, p.warn));
    let checked = match &updater.status.checked {
        Some(at) => format!("last checked {at}"),
        None => "not checked yet".to_string(),
    };
    hint(ui, format!("v{} · {checked}", updater.build.version), p.t3);
}

// ---- the sidebar's update button ----------------------------------------------------------------

const BUTTON_H: f32 = 34.0;
const BUTTON_MARGIN: f32 = 12.0;
/// How long the button takes to rise in, and to sink away.
const RISE: f32 = 0.55;
const SINK: f32 = 0.22;
/// How far below its place it starts.
const RISE_FROM: f32 = 18.0;
/// The label's letters come in one after another, each sliding up from this far.
const LETTER_DELAY: f64 = 0.022;
const LETTER_IN: f64 = 0.32;
const LETTER_FROM: f32 = 9.0;
/// The label it replaces slides up and fades in this long.
const LETTER_OUT: f64 = 0.16;
/// One light sweep across the button once the letters are in.
const SHIMMER: f64 = 0.85;
/// The download's fill follows the percentage this smoothly.
const FILL: f32 = 0.45;

/// What the button says and does for a state; None when there's nothing to take.
#[derive(Clone, Debug, PartialEq)]
enum Offer {
    Available(String),
    Downloading(String, u8),
    Ready(String),
}

impl Offer {
    fn from(state: &State) -> Option<Offer> {
        match state {
            State::Available { version } => Some(Offer::Available(version.clone())),
            State::Downloading { version, percent } => Some(Offer::Downloading(version.clone(), *percent)),
            State::Ready { version } => Some(Offer::Ready(version.clone())),
            _ => None,
        }
    }

    /// Changes of this restart the label's entrance; a download's progress doesn't.
    fn key(&self) -> String {
        match self {
            Offer::Available(v) => format!("available {v}"),
            Offer::Downloading(v, _) => format!("downloading {v}"),
            Offer::Ready(v) => format!("ready {v}"),
        }
    }

    fn label(&self, percent: u8) -> String {
        match self {
            Offer::Available(v) => format!("update to {v}"),
            Offer::Downloading(..) => format!("downloading {percent}%"),
            Offer::Ready(_) => "restart to update".into(),
        }
    }
}

/// When the label last changed, and what it said before.
#[derive(Clone)]
struct Entrance {
    key: String,
    since: f64,
    before: Option<String>,
}

/// The design's ease-out with a slight overshoot past 1 before it settles.
fn overshoot(t: f32) -> f32 {
    const C1: f32 = 1.3;
    const C3: f32 = C1 + 1.0;
    let u = t.clamp(0.0, 1.0) - 1.0;
    1.0 + C3 * u * u * u + C1 * u * u
}

/// The update button at the bottom of the sidebar `side`: it rises in while there's an update
/// to take ("update to 0.3.0"), fills while it downloads ("downloading 42%") and offers the
/// restart once it's ready; clicking asks the tray app to install.
pub fn sidebar_button(ui: &mut Ui, look: &Look, side: Rect, updater: &mut Updater) {
    let p = look.p;
    let ctx = ui.ctx().clone();
    updater.poll();
    let offer = Offer::from(&updater.status.state);
    let downloading = matches!(offer, Some(Offer::Downloading(..)));
    ctx.request_repaint_after(Duration::from_millis(if downloading { 150 } else { 500 }));
    let id = Id::unique("update-button");
    let shown = offer.is_some();
    let t = widgets::anim(ui, id.with("shown"), shown, if shown { RISE } else { SINK });
    let last_id = id.with("last");
    let entrance_id = id.with("entrance");
    let offer = match offer {
        Some(o) => {
            ctx.data_mut(|d| d.insert_temp(last_id, o.clone()));
            o
        }
        // folding away still shows what it said
        None => match ctx.data(|d| d.get_temp::<Offer>(last_id)) {
            Some(o) if t > 0.0 => o,
            // gone: the next one comes in afresh
            _ => return ctx.data_mut(|d| d.remove::<Entrance>(entrance_id)),
        },
    };
    let now = ctx.input(|i| i.time);
    let percent = match &offer {
        Offer::Downloading(_, pct) => *pct,
        _ => 100,
    };
    let key = offer.key();
    let mut entrance = ctx.data(|d| d.get_temp::<Entrance>(entrance_id));
    if entrance.as_ref().is_none_or(|e| e.key != key) {
        let before = entrance.as_ref().and_then(|_| ctx.data(|d| d.get_temp::<String>(id.with("label"))));
        let e = Entrance { key, since: now, before: before.filter(|_| t >= 1.0) };
        ctx.data_mut(|d| d.insert_temp(entrance_id, e.clone()));
        // a download's fill sweeps in from the left
        widgets::snap(&ctx, id.with("percent"), if downloading { 0.0 } else { percent as f32 });
        entrance = Some(e);
    }
    let entrance = entrance.expect("set above");
    // the number ticks up toward each new percentage instead of jumping
    let shown_pct = widgets::glide(ui, id.with("percent"), percent as f32, FILL);
    let label = offer.label(shown_pct.round() as u8);
    ctx.data_mut(|d| d.insert_temp(id.with("label"), label.clone()));

    // rises with a slight overshoot; sinks without one
    let lift = if shown { overshoot(t) } else { ease(t) };
    let fade = ease(t);
    let full = Rect::from_min_size(
        pos2(side.left() + BUTTON_MARGIN, side.bottom() - BUTTON_MARGIN - BUTTON_H),
        vec2(side.width() - 2.0 * BUTTON_MARGIN, BUTTON_H),
    );
    let scale = 0.92 + 0.08 * lift;
    let rect = Rect::from_center_size(full.center() + vec2(0.0, (1.0 - lift) * RISE_FROM), full.size() * scale);

    let enabled = shown && !downloading && t >= 1.0;
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let resp = ui.interact(full, id, sense);
    let h = widgets::anim(ui, id.with("hov"), enabled && resp.hovered(), 0.08);
    let d = widgets::anim(ui, id.with("prs"), enabled && resp.is_pointer_button_down_on(), 0.08);

    let mut painter = ui.painter().with_clip_rect(side);
    painter.multiply_opacity(fade);
    if let Some(note) = &updater.note {
        let galley = painter.layout(note.clone(), sans(11.0), p.warn, full.width());
        painter.galley(pos2(full.left() + 2.0, rect.top() - 8.0 - galley.size().y), galley, p.warn);
    }
    let radius = rect.height() / 2.0;
    let fill = mix(mix(p.acc, p.acch, h), p.accp, d);
    let split = if downloading { rect.left() + rect.width() * (shown_pct / 100.0).clamp(0.0, 1.0) } else { rect.right() };
    if downloading {
        // the track: the accent faded into the sidebar, then the fill sweeping across it
        painter.rect_filled(rect, radius, mix(p.side, p.acc, 0.28));
        let done = Rect::from_min_max(rect.min, pos2(split, rect.bottom()));
        painter.with_clip_rect(done.intersect(side)).rect_filled(rect, radius, fill);
    } else {
        painter.rect_filled(rect, radius, fill);
    }

    let font = medium(13.0);
    let motion = motion();
    let age = now - entrance.since;
    // a label that replaces another waits for most of the old one to leave
    let lead = if entrance.before.is_some() { LETTER_OUT * 0.6 } else { 0.0 };
    let mut busy = false;
    // over the fill the label reads on the accent, over the track on the sidebar
    let halves = [
        (Rect::from_min_max(rect.min, pos2(split, rect.bottom())), p.on_acc),
        (Rect::from_min_max(pos2(split, rect.top()), rect.max), p.t1),
    ];
    for (area, color) in halves {
        if area.width() <= 0.0 {
            continue;
        }
        let painter = painter.with_clip_rect(area.intersect(side));
        if motion {
            busy |= letters(&painter, rect, &label, font.clone(), color, age - lead);
            if let Some(before) = &entrance.before
                && age < LETTER_OUT
            {
                let k = (age / LETTER_OUT) as f32;
                let galley = painter.layout_no_wrap(before.clone(), font.clone(), color);
                let at = rect.center() - galley.size() / 2.0 - vec2(0.0, k * LETTER_FROM * 0.7);
                painter.galley(at, galley, color.gamma_multiply(1.0 - ease(k)));
                busy = true;
            }
        } else {
            let galley = painter.layout_no_wrap(label.clone(), font.clone(), color);
            painter.galley(rect.center() - galley.size() / 2.0, galley, color);
        }
    }
    if motion && !downloading {
        let landed = lead + LETTER_DELAY * label.chars().count() as f64 + LETTER_IN * 0.6;
        let k = (age - landed) / SHIMMER;
        if (0.0..1.0).contains(&k) {
            shimmer(&painter, rect, k as f32);
            busy = true;
        } else if k < 0.0 {
            busy = true;
        }
    }
    if busy || (t > 0.0 && t < 1.0) {
        ctx.request_repaint();
    }

    focus_ring(ui, look, &resp, rect, radius);
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, &label));
    let resp = if enabled { resp.on_hover_cursor(CursorIcon::PointingHand) } else { resp };
    if resp.clicked() {
        updater.install();
    }
}

/// Paints `label` centred in `rect`, its letters coming in one after another `age` seconds
/// after it changed; whether any is still moving.
fn letters(painter: &Painter, rect: Rect, label: &str, font: eframe::egui::FontId, color: Color32, age: f64) -> bool {
    let whole = painter.layout_no_wrap(label.to_owned(), font.clone(), color);
    let mut x = rect.center().x - whole.size().x / 2.0;
    let y = rect.center().y - whole.size().y / 2.0;
    let mut moving = false;
    for (i, ch) in label.chars().enumerate() {
        let glyph = painter.layout_no_wrap(ch.to_string(), font.clone(), color);
        let k = ((age - LETTER_DELAY * i as f64) / LETTER_IN).clamp(0.0, 1.0) as f32;
        moving |= k < 1.0;
        if k > 0.0 {
            let rise = (1.0 - overshoot(k)) * LETTER_FROM;
            painter.galley(pos2(x, y + rise), glyph.clone(), color.gamma_multiply(ease(k.min(1.0) * 1.4).min(1.0)));
        }
        x += glyph.size().x;
    }
    moving
}

/// A soft slanted band of light `k` (0..1) of the way across the pill `rect`, drawn as vertical
/// strips that follow the pill's outline so it never spills past the round ends.
fn shimmer(painter: &Painter, rect: Rect, k: f32) {
    let r = rect.height() / 2.0;
    let band = rect.height() * 1.5;
    let centre = rect.left() - band + (rect.width() + 2.0 * band) * ease(k);
    let slant = rect.height() * 0.4;
    let peak = 72.0 * (1.0 - k).sqrt();
    let light = |x: f32| {
        let d = ((x - centre).abs() / (band / 2.0)).min(1.0);
        Color32::from_white_alpha((peak * (1.0 - d) * (1.0 - d)) as u8)
    };
    let strips = (rect.width() / 3.0).ceil().max(1.0) as u32;
    let mut mesh = Mesh::default();
    for i in 0..=strips {
        let x = rect.left() + rect.width() * i as f32 / strips as f32;
        let dx = (rect.left() + r - x).max(x - (rect.right() - r)).max(0.0);
        let h = (r * r - dx * dx).max(0.0).sqrt();
        let lean = slant * h / r;
        mesh.colored_vertex(pos2(x, rect.center().y - h), light(x - lean));
        mesh.colored_vertex(pos2(x, rect.center().y + h), light(x + lean));
    }
    for i in 0..strips {
        let b = i * 2;
        mesh.add_triangle(b, b + 1, b + 2);
        mesh.add_triangle(b + 1, b + 3, b + 2);
    }
    painter.add(Shape::mesh(mesh));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_button_says_what_taking_the_update_does() {
        let at = |state: State| Offer::from(&state);
        let v = || "0.3.0".to_string();
        assert_eq!(at(State::Available { version: v() }).unwrap().label(100), "update to 0.3.0");
        let down = at(State::Downloading { version: v(), percent: 42 }).unwrap();
        assert_eq!(down.label(42), "downloading 42%");
        assert_eq!(down.key(), at(State::Downloading { version: v(), percent: 90 }).unwrap().key(), "progress keeps the label in");
        assert_eq!(at(State::Ready { version: v() }).unwrap().label(100), "restart to update");
        for none in [State::Unknown, State::Checking, State::UpToDate, State::Failed { error: "x".into() }] {
            assert_eq!(at(none), None);
        }
    }

    #[test]
    fn the_rise_overshoots_a_little_then_settles() {
        assert_eq!(overshoot(0.0), 0.0);
        assert!((overshoot(1.0) - 1.0).abs() < 1e-6);
        let peak = (1..100).map(|i| overshoot(i as f32 / 100.0)).fold(0.0, f32::max);
        assert!(peak > 1.02 && peak < 1.08, "{peak}");
    }
}
