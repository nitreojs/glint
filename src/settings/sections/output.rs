use std::path::{Path, PathBuf};

use eframe::egui::{self, RichText, Ui};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Shell::{
    FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName,
    SIGDN_FILESYSPATH,
};
use windows::core::HSTRING;

use crate::app::ipc;
use crate::config::{Config, DEFAULT_FILE_NAME, file_name_problem};
use crate::platform::format::date_time;
use crate::settings::SettingsApp;
use crate::settings::widgets::{self, Kind, Look, hint, mono, row};

fn pick_folder(start: &Path) -> Option<PathBuf> {
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let opts = dialog.GetOptions().ok()?;
        dialog.SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
        if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(start.as_os_str()), None) {
            let _ = dialog.SetFolder(&item);
        }
        let parent: Option<HWND> = ipc::find_settings();
        dialog.Show(parent).ok()?; // Err when cancelled
        let path = dialog.GetResult().ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = path.to_string().ok();
        CoTaskMemFree(Some(path.0 as _));
        s.map(PathBuf::from)
    }
}

impl SettingsApp {
    pub(in crate::settings) fn output(&mut self, ui: &mut Ui, look: &Look) {
        let p = look.p;
        let default_dir = Config { output_dir: String::new(), ..Config::default() }.output_dir();
        row(ui, look, "folder", 7.0, |ui| {
            widgets::text_field(ui, look, &mut self.cfg.output_dir, &default_dir.display().to_string(), false);
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if widgets::button(ui, look, "browse…", Kind::Secondary, 28.0, true).clicked()
                    && let Some(dir) = pick_folder(&self.cfg.output_dir())
                {
                    self.cfg.output_dir = dir.display().to_string();
                }
                if widgets::button(ui, look, "open folder", Kind::Secondary, 28.0, true).clicked() {
                    let dir = self.cfg.output_dir();
                    let _ = std::fs::create_dir_all(&dir);
                    let _ = std::process::Command::new("explorer.exe").arg(&dir).spawn();
                }
            });
            ui.add_space(2.0);
            hint(ui, "recordings and screenshots are saved here. leave empty for the default.", p.t3);
        });
        let problem = file_name_problem(&self.cfg.file_name);
        row(ui, look, "file names", 7.0, |ui| {
            widgets::text_field(ui, look, &mut self.cfg.file_name, DEFAULT_FILE_NAME, problem.is_some());
            let (date, time) = date_time();
            let example = format!("{}.mp4", self.cfg.file_stem(&date, &time));
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.label(RichText::new("→").font(mono(12.0)).color(p.t3));
                ui.add(egui::Label::new(RichText::new(example).font(mono(12.0)).color(p.t2)).truncate());
            });
            match problem {
                Some(msg) => hint(ui, msg, p.warn),
                None => hint(ui, "{date} and {time} are filled in when the file is saved", p.t3),
            }
        });
    }
}
