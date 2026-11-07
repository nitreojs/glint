//! Original cleanup is deferred until a deliberate window close, never a file swap or Drop.

use std::path::{Path, PathBuf};

use eframe::egui;
use glint_core::config::AfterTrim;
use glint_core::clip::edit::Edit;
use glint_core::gallery::path_id;
use glint_core::log::log;

use super::{ClipApp, Note};

pub(super) struct Trimmed {
    original: PathBuf,
    output: PathBuf,
    /// None after a later export failed/cancelled, or edits were left unsaved.
    edit: Option<Edit>,
}

/// Only originals with a separate, nonempty saved copy still on disk. Every saved output is
/// protected, including when it was subsequently opened as another edit's source.
fn originals_to_recycle(trimmed: &[Trimmed]) -> Vec<PathBuf> {
    let outputs: Vec<_> = trimmed.iter().map(|t| {
        let meta = std::fs::metadata(&t.output).ok()?;
        (meta.is_file() && meta.len() > 0).then(|| std::fs::canonicalize(&t.output).ok()).flatten()
    }).collect();
    trimmed.iter().zip(&outputs).filter_map(|(t, output)| {
        t.edit.as_ref()?;
        output.as_ref()?;
        let original = std::fs::canonicalize(&t.original).ok()?;
        (!outputs.iter().flatten().any(|p| *p == original)).then(|| t.original.clone())
    }).collect()
}

impl ClipApp {
    pub(super) fn remember_trim(&mut self, output: &Path, edit: Edit) {
        if self.after_trim == AfterTrim::KeepBoth {
            return;
        }
        let source = path_id(&self.path);
        if let Some(t) = self.trimmed.iter_mut().find(|t| path_id(&t.original) == source) {
            t.output = output.to_path_buf();
            t.edit = Some(edit);
        } else {
            self.trimmed.push(Trimmed { original: self.path.clone(), output: output.to_path_buf(), edit: Some(edit) });
        }
    }

    /// Keep older outputs protected, but a failed new save must not authorize cleanup.
    pub(super) fn forget_trim(&mut self) {
        if self.trimmed.is_empty() {
            return;
        }
        let source = path_id(&self.path);
        if let Some(t) = self.trimmed.iter_mut().find(|t| path_id(&t.original) == source) {
            t.edit = None;
        }
    }

    /// Compare with the edit captured when export began, not when its worker finished.
    pub(super) fn check_trim(&mut self) {
        if self.trimmed.is_empty() {
            return;
        }
        let source = path_id(&self.path);
        if let Some(t) = self.trimmed.iter_mut().find(|t| path_id(&t.original) == source)
            && t.edit.as_ref() != Some(&self.edit)
        {
            t.edit = None;
        }
    }

    pub(super) fn close(&mut self, ctx: &egui::Context) {
        self.closing = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    pub(super) fn request_close(&mut self, ctx: &egui::Context) {
        // A close during an export keeps originals, even if an earlier edit was saved.
        if self.closing || self.gone || self.busy() || self.after_trim == AfterTrim::KeepBoth {
            return self.close(ctx);
        }
        self.check_trim();
        let originals = originals_to_recycle(&self.trimmed);
        if originals.is_empty() {
            return self.close(ctx);
        }
        self.pause();
        self.confirm_cleanup = Some(originals);
        if self.after_trim == AfterTrim::KeepTrimmed {
            self.recycle_trimmed(ctx);
        }
    }

    pub(super) fn release_media(&mut self) {
        self.pause();
        self.player = None;
        self.audio = None;
        self.scan = None;
        self.waves = None;
    }

    pub(super) fn recycle_trimmed(&mut self, ctx: &egui::Context) {
        self.release_media();
        // Recheck after the dialog: a copy may have been moved/deleted while it was open.
        for original in originals_to_recycle(&self.trimmed) {
            if let Err(e) = glint_shell::explorer::recycle(self.owner(), &original) {
                log(&format!("clip: recycling {} failed: {e}", original.display()));
                self.toast(Note::Failed(format!("couldn't recycle {}: {e}", original.display())));
                self.confirm_cleanup = Some(originals_to_recycle(&self.trimmed));
                return;
            }
        }
        self.confirm_cleanup = None;
        self.close(ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Files(PathBuf);

    impl Files {
        fn new() -> Self {
            let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let dir = std::env::temp_dir().join(format!("glint-cleanup-{}-{stamp}", std::process::id()));
            std::fs::create_dir(&dir).unwrap();
            Self(dir)
        }

        fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        }
    }

    impl Drop for Files {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn app(path: PathBuf, policy: AfterTrim) -> ClipApp {
        let mut app = ClipApp::new(None, 50, policy, String::new(), super::super::Saved::default(), [229, 64, 47]);
        app.path = path;
        app
    }

    #[test]
    fn only_completed_edited_exports_offer_cleanup_and_survive_file_switches() {
        use super::super::{Opened, Task, Then};
        use std::sync::{Arc, atomic::{AtomicBool, AtomicU32}};
        let f = Files::new();
        let original = f.file("original.mp4", b"original");
        let ctx = egui::Context::default();
        for (result, cancelled, copied, changed) in [
            (Ok(false), false, false, false),
            (Err("export failed".to_string()), false, false, false),
            (Ok(true), false, true, false),
            (Ok(true), true, false, false),
            (Ok(true), false, false, true),
            (Ok(true), false, false, false),
        ] {
            let mut app = app(original.clone(), AfterTrim::Ask);
            let output = f.file("trimmed.mp4", b"edited bytes");
            let succeeded = result == Ok(true);
            let previous = f.file("previous.mp4", b"earlier successful edit");
            app.remember_trim(&previous, app.edit.clone());
            app.forget_trim();
            let thread = std::thread::spawn(move || result);
            while !thread.is_finished() {
                std::thread::yield_now();
            }
            app.task = Some(Task {
                out: output.clone(), dest: None, then: Then::Nothing, copy: copied, before: None,
                edit: (!copied).then(|| app.edit.clone()),
                permille: Arc::new(AtomicU32::new(1000)), cancel: Arc::new(AtomicBool::new(cancelled)), thread: Some(thread),
            });
            if changed {
                app.edit.markers.push(1);
            }
            app.poll_task(&ctx);
            let next = Opened { path: f.file("next.mp4", b"next"), info: Err(String::new()), index: None };
            app.swap_in(&ctx, next, None);
            app.request_close(&ctx);
            if succeeded && !cancelled && !copied && !changed {
                assert_eq!(app.confirm_cleanup.as_deref(), Some([original.clone()].as_slice()));
                assert!(!app.closing);
            } else {
                assert!(app.confirm_cleanup.is_none() && app.closing);
            }
            if succeeded {
                assert_eq!(std::fs::read(&output).unwrap(), b"edited bytes");
            } else {
                assert!(!output.exists(), "failed/cancelled output is removed, not the original");
            }
            assert_eq!(std::fs::read(&original).unwrap(), b"original");
        }
    }

    #[test]
    fn unsaved_followup_edits_keep_the_original_even_after_switching_files() {
        let f = Files::new();
        let original = f.file("original.mp4", b"original");
        let output = f.file("trimmed.mp4", b"edited");
        let ctx = egui::Context::default();
        for switch in [false, true] {
            let mut app = app(original.clone(), AfterTrim::Ask);
            app.remember_trim(&output, app.edit.clone());
            app.edit.markers.push(1);
            if switch {
                let next = super::super::Opened { path: f.file("next.mp4", b"next"), info: Err(String::new()), index: None };
                app.swap_in(&ctx, next, None);
            }
            app.request_close(&ctx);
            assert!(app.closing && app.confirm_cleanup.is_none());
            assert_eq!(std::fs::read(&original).unwrap(), b"original");
            assert_eq!(std::fs::read(&output).unwrap(), b"edited");
        }
    }

    #[test]
    fn keep_both_and_unsaved_closes_leave_the_original_alone() {
        let f = Files::new();
        let original = f.file("original.mp4", b"original");
        let output = f.file("trimmed.mp4", b"edited");
        let ctx = egui::Context::default();
        for policy in [AfterTrim::KeepBoth, AfterTrim::KeepTrimmed, AfterTrim::Ask] {
            let mut app = app(original.clone(), policy);
            if policy == AfterTrim::KeepBoth {
                app.remember_trim(&output, app.edit.clone());
            }
            app.request_close(&ctx);
            assert!(app.closing && app.confirm_cleanup.is_none());
            assert_eq!(std::fs::read(&original).unwrap(), b"original");
        }
    }

    #[test]
    fn a_saved_copy_removed_while_asking_cannot_trigger_recycling() {
        let f = Files::new();
        let original = f.file("original.mp4", b"original");
        let output = f.file("trimmed.mp4", b"edited");
        let ctx = egui::Context::default();
        let mut app = app(original.clone(), AfterTrim::Ask);
        app.remember_trim(&output, app.edit.clone());
        app.request_close(&ctx);
        assert!(app.confirm_cleanup.is_some() && !app.closing);
        std::fs::remove_file(output).unwrap();
        app.recycle_trimmed(&ctx);
        assert!(app.closing && app.confirm_cleanup.is_none());
        assert_eq!(std::fs::read(original).unwrap(), b"original");
    }

    #[test]
    fn cleanup_requires_a_separate_nonempty_copy_that_still_exists() {
        let f = Files::new();
        let original = f.file("original.mp4", b"original");
        let output = f.file("trimmed.mp4", b"trimmed");
        let trimmed = [Trimmed { original: original.clone(), output: output.clone(), edit: Some(Edit::new(0, 0)) }];
        assert_eq!(originals_to_recycle(&trimmed), std::slice::from_ref(&original));
        std::fs::write(&output, b"").unwrap();
        assert!(originals_to_recycle(&trimmed).is_empty());
        std::fs::remove_file(&output).unwrap();
        assert!(originals_to_recycle(&trimmed).is_empty());
        let alias = f.0.join(".").join("original.mp4");
        assert!(originals_to_recycle(&[Trimmed { original: original.clone(), output: alias, edit: Some(Edit::new(0, 0)) }]).is_empty());
        assert_eq!(std::fs::read(original).unwrap(), b"original");
    }

    #[test]
    fn saved_outputs_are_protected_even_when_opened_as_sources() {
        let f = Files::new();
        let a = f.file("a.mp4", b"a");
        let b = f.file("b.mp4", b"b");
        let c = f.file("c.mp4", b"c");
        let mut trimmed = vec![
            Trimmed { original: a.clone(), output: b.clone(), edit: Some(Edit::new(0, 0)) },
            Trimmed { original: b, output: c.clone(), edit: Some(Edit::new(0, 0)) },
        ];
        assert_eq!(originals_to_recycle(&trimmed), std::slice::from_ref(&a));
        trimmed.push(Trimmed { original: c, output: a, edit: Some(Edit::new(0, 0)) });
        assert!(originals_to_recycle(&trimmed).is_empty(), "a cycle must not delete every copy");
    }
}
