use std::borrow::Cow;
use std::path::PathBuf;
use std::thread::JoinHandle;

use eframe::egui::{
    self, Color32, ColorImage, CursorIcon, Id, Painter, Rect, Response, Role, Sense, Shape, Stroke,
    StrokeKind, TextureHandle, TextureOptions, Ui, WidgetInfo, epaint::RectShape, pos2, vec2,
};
use glint_core::clip::export;
use glint_core::log::log;
use glint_media::clip::media::Com;
use glint_shell::{clipboard, drag};
use glint_ui::widgets::{Look, sans, semibold};
use windows::Win32::Foundation::HWND;

use super::{ClipApp, Note, actions};

pub(super) const FILE_ID: &str = "clip-toast-file";
pub(super) const LINK_ID: &str = "clip-toast-link";
pub(super) const LAST_ID: &str = "clip-last-export";
const HINT: &str = "drag to share · click to copy";

pub(super) enum Action {
    Copy,
    Drag,
    Open,
}

pub(super) struct SavedFile {
    pub path: PathBuf,
    subtitle: String,
    comparison: Option<String>,
    pub dragged: bool,
    loading: Option<JoinHandle<Option<(ColorImage, drag::Image)>>>,
    picture: Option<(TextureHandle, drag::Image)>,
}

impl SavedFile {
    pub fn new(path: PathBuf, size: Option<u64>, before: Option<u64>) -> Self {
        let src = path.clone();
        let loading = std::thread::spawn(move || {
            let _com = Com::init();

            glint_ui::gallery::drag_preview(&src, (160, 100))
                .map_err(|e| log(&format!("clip: no preview for {}: {e}", src.display())))
                .ok()
        });

        let (subtitle, comparison) = match size {
            Some(bytes) => {
                let (size, comparison) = export::saved_size(before, bytes);

                (format!("{size} · {HINT}"), comparison)
            }
            None => (HINT.into(), None),
        };

        Self { path, subtitle, comparison, dragged: false, loading: Some(loading), picture: None }
    }

    fn name(&self) -> Cow<'_, str> {
        self.path.file_name().unwrap_or(self.path.as_os_str()).to_string_lossy()
    }

    fn poll_picture(&mut self, ctx: &egui::Context) {
        let Some(task) = &self.loading else { return };
        if !task.is_finished() {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));

            return;
        }

        let Some(done) = self.loading.take() else { return };
        if let Some((image, drag)) = done.join().ok().flatten() {
            self.picture = Some((ctx.load_texture("saved-file", image, TextureOptions::LINEAR), drag));
        }
    }

    pub fn card(&mut self, ui: &mut Ui, look: &Look, width: f32) -> Option<Action> {
        const INSET: f32 = 70.0;
        const FOLDER_W: f32 = 40.0;
        self.poll_picture(ui.ctx());
        let p = look.p;
        let painter = ui.painter();
        let room = (width - INSET - 12.0 - FOLDER_W).clamp(1.0, 440.0);
        let title = painter.layout_job(egui::text::LayoutJob {
            wrap: egui::text::TextWrapping::truncate_at_width(room),
            ..egui::text::LayoutJob::single_section(
                self.name().into_owned(),
                egui::TextFormat::simple(semibold(13.0), p.t1),
            )
        });
        let hint = painter.layout_no_wrap(self.subtitle.clone(), sans(12.0), p.t2);
        let text_w = title.size().x.max(hint.size().x);
        let text_h = title.size().y + 2.0 + hint.size().y;
        let size = vec2(INSET + text_w + 12.0 + FOLDER_W, text_h.max(34.0) + 16.0);
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let file = Rect::from_min_max(rect.min, pos2(rect.right() - FOLDER_W, rect.bottom()));
        let link = Rect::from_min_max(file.right_top(), rect.max);
        let mut resp = file_button(ui, file, FILE_ID, "copy saved file or drag to share");
        if title.elided || self.comparison.is_some() {
            resp = resp.on_hover_ui(|ui| {
                if title.elided {
                    ui.label(self.name().as_ref());
                }
                if let Some(detail) = &self.comparison {
                    ui.label(detail);
                }
            });
        }
        let open = ui.interact(link, Id::unique(LINK_ID), Sense::click())
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text("show in folder");
        open.widget_info(|| WidgetInfo::labeled(Role::Link, true, "show in folder"));

        let painter = ui.painter();
        painter.rect(rect, 8, p.ctl, Stroke::new(1.0, p.line2), StrokeKind::Inside);
        if resp.hovered() || (look.keyboard && resp.has_focus()) {
            painter.rect_filled(file.shrink(3.0), 6, p.ctlh);
        }
        let tile = Rect::from_center_size(pos2(rect.left() + 36.0, rect.center().y), vec2(52.0, 34.0));
        self.paint_tile(painter, look, tile);
        let at = pos2(rect.left() + INSET, rect.center().y - text_h / 2.0);
        let below = at + vec2(0.0, title.size().y + 2.0);
        painter.galley(at, title, p.t1);
        painter.galley(below, hint, p.t2);
        painter.vline(link.left(), rect.top() + 9.0..=rect.bottom() - 9.0, Stroke::new(1.0, p.line2));
        let ink = if open.hovered() || (look.keyboard && open.has_focus()) { p.t1 } else { p.t2 };
        let c = link.center();
        painter.add(Shape::closed_line(
            vec![
                c + vec2(-8.5, -6.0),
                c + vec2(-2.5, -6.0),
                c + vec2(0.0, -3.5),
                c + vec2(8.5, -3.5),
                c + vec2(8.5, 6.0),
                c + vec2(-8.5, 6.0),
            ],
            Stroke::new(1.5, ink),
        ));

        if open.clicked() { Some(Action::Open) } else { self.gesture(&resp) }
    }

    pub fn compact(&mut self, ui: &mut Ui, look: &Look, rect: Rect) -> Option<Action> {
        self.poll_picture(ui.ctx());
        let resp = file_button(ui, rect, LAST_ID, "last export: copy file or drag to share").on_hover_ui(|ui| {
            ui.strong(format!("last export: {}", self.name()));
            if let Some(comparison) = &self.comparison {
                ui.label(comparison);
                ui.label(HINT);
            } else {
                ui.label(&self.subtitle);
            }
        });
        let p = look.p;
        let border = if resp.hovered() || (look.keyboard && resp.has_focus()) { p.t2 } else { p.line2 };
        ui.painter().rect(rect, 8, p.ctl, Stroke::new(1.0, border), StrokeKind::Inside);
        self.paint_tile(ui.painter(), look, rect.shrink(4.0));

        self.gesture(&resp)
    }

    fn paint_tile(&self, painter: &Painter, look: &Look, tile: Rect) {
        let p = look.p;
        painter.rect_filled(tile, 4, p.surf);
        if let Some((tex, _)) = &self.picture {
            let size = tex.size_vec2();
            let scale = (tile.width() / size.x).min(tile.height() / size.y);
            let pic = Rect::from_center_size(tile.center(), size * scale);
            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            painter.add(RectShape::filled(pic, 4, Color32::WHITE).with_texture(tex.id(), uv));
        } else {
            let c = tile.center();
            painter.add(Shape::convex_polygon(
                vec![c + vec2(-3.0, -5.0), c + vec2(5.0, 0.0), c + vec2(-3.0, 5.0)],
                p.t2,
                Stroke::NONE,
            ));
        }
    }

    fn gesture(&mut self, resp: &egui::Response) -> Option<Action> {
        if !resp.ctx.input(|i| i.pointer.primary_down()) {
            let was_drag = std::mem::take(&mut self.dragged);

            return (resp.clicked() && !was_drag).then_some(Action::Copy);
        }

        if !self.dragged && resp.is_pointer_button_down_on() {
            let delta = resp.ctx.input(|i| Some(i.pointer.interact_pos()? - i.pointer.press_origin()?))?;
            let px = delta * resp.ctx.pixels_per_point();
            if drag::started(px.x, px.y, drag::threshold()) {
                self.dragged = true;

                return Some(Action::Drag);
            }
        }

        None
    }
}

fn file_button(ui: &Ui, rect: Rect, key: &'static str, label: &'static str) -> Response {
    let resp = ui.interact(rect, Id::unique(key), Sense::click_and_drag()).on_hover_cursor(CursorIcon::Grab);
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));

    resp
}

pub(super) fn copy_file(owner: Option<HWND>, path: &PathBuf) -> Note {
    match clipboard::copy_files(owner, std::slice::from_ref(path)) {
        Ok(()) => Note::Copied,
        Err(e) => Note::Failed(format!("couldn't copy the file: {e}")),
    }
}

impl ClipApp {
    pub(super) fn share_export(&mut self, action: Action) {
        let Some(file) = &self.last_export else { return };
        let path = match std::path::absolute(&file.path) {
            Ok(path) if path.is_file() => path,
            _ => {
                self.toast(Note::Failed(format!("can't find the saved file: {}", file.path.display())));

                return;
            }
        };
        match action {
            Action::Open => actions::show_in_folder(&path),
            Action::Copy => self.toast(copy_file(self.hwnd, &path)),
            Action::Drag => {
                let image = file.picture.as_ref().map(|(_, image)| image);
                if let Err(e) = drag::files(self.hwnd, std::slice::from_ref(&path), image) {
                    self.toast(Note::Failed(format!("couldn't drag the file: {e}")));
                }
            }
        }
    }
}
