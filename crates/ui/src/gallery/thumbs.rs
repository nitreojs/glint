//! Thumbnails made off the UI thread: a few workers take what's in view first, read it from the
//! disk cache (`%LOCALAPPDATA%\<app>\thumbs`) or make it (WIC for pictures, Media Foundation on the
//! GPU for recordings) and cache it. Screenshot previews decode at their shown size the same way,
//! ahead of every thumbnail.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

use eframe::egui::{self, ColorImage};
use glint_core::clip::math::SECOND;
use glint_core::gallery::Kind;
use glint_core::gallery::thumb::{self, Meta};
use glint_core::log::log;
use glint_media::clip::media::{self, Com, Gpu};
use glint_media::image::{ImageFormat, encode};

use super::decode::{Pixels, Wic};
use super::play;

/// A thumbnail's shorter side and the most its longer side gets, in pixels.
const SHORT: u32 = 280;
const LONG: u32 = 1024;
const WORKERS: usize = 3;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Job {
    pub id: u64,
    pub key: u64,
    pub path: PathBuf,
    pub kind: Kind,
}

pub(super) enum Done {
    /// `image` is `None` when it couldn't be read.
    Thumb { id: u64, image: Option<ColorImage>, meta: Meta },
    Full { id: u64, image: Option<ColorImage> },
}

struct Full {
    id: u64,
    path: PathBuf,
    /// The box it's shown in, in pixels.
    size: (u32, u32),
}

#[derive(Default)]
struct Queue {
    wanted: VecDeque<Job>,
    /// Being made right now: asked for again, they aren't made twice.
    busy: HashSet<u64>,
    full: Option<Full>,
    quit: bool,
}

pub(super) struct Pool {
    shared: Arc<(Mutex<Queue>, Condvar)>,
    rx: Receiver<Done>,
    /// What was last asked for, so an unchanged view doesn't touch the lock.
    asked: Vec<u64>,
}

pub(super) fn cache_dir(app: &str) -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join(app).join("thumbs")
}

impl Pool {
    pub fn start(ctx: &egui::Context, dir: PathBuf) -> Pool {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            log(&format!("gallery: no thumbnail cache at {}: {e}", dir.display()));
        }
        let shared = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let (tx, rx) = mpsc::channel();
        for n in 0..WORKERS {
            let (shared, tx, ctx, dir) = (shared.clone(), tx.clone(), ctx.clone(), dir.clone());
            let spawned = std::thread::Builder::new().name(format!("thumbs {n}")).spawn(move || work(&shared, &dir, &tx, &ctx));
            if let Err(e) = spawned {
                log(&format!("gallery: can't start a thumbnail worker: {e}"));
            }
        }
        Pool { shared, rx, asked: Vec::new() }
    }

    /// What's missing in view, nearest first; whatever was asked before and isn't started is dropped.
    pub fn want(&mut self, jobs: Vec<Job>) {
        let ids: Vec<u64> = jobs.iter().map(|j| j.id).collect();
        if ids == self.asked {
            return;
        }
        self.asked = ids;
        let (lock, cv) = &*self.shared;
        if let Ok(mut q) = lock.lock() {
            q.wanted = jobs.into();
            cv.notify_all();
        }
    }

    /// A picture at `size` pixels for the preview, before any thumbnail.
    pub fn full(&self, id: u64, path: PathBuf, size: (u32, u32)) {
        let (lock, cv) = &*self.shared;
        if let Ok(mut q) = lock.lock() {
            q.full = Some(Full { id, path, size });
            cv.notify_one();
        }
    }

    pub fn poll(&self) -> Option<Done> {
        self.rx.try_recv().ok()
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        let (lock, cv) = &*self.shared;
        if let Ok(mut q) = lock.lock() {
            q.quit = true;
            q.wanted.clear();
        }
        cv.notify_all();
    }
}

enum Task {
    Thumb(Job),
    Full(Full),
}

fn next(shared: &(Mutex<Queue>, Condvar)) -> Option<Task> {
    let (lock, cv) = shared;
    let mut q = lock.lock().ok()?;
    loop {
        if q.quit {
            return None;
        }
        if let Some(f) = q.full.take() {
            return Some(Task::Full(f));
        }
        while let Some(j) = q.wanted.pop_front() {
            if q.busy.insert(j.id) {
                return Some(Task::Thumb(j));
            }
        }
        q = cv.wait(q).ok()?;
    }
}

fn finished(shared: &(Mutex<Queue>, Condvar), id: u64) {
    if let Ok(mut q) = shared.0.lock() {
        q.busy.remove(&id);
    }
}

fn work(shared: &(Mutex<Queue>, Condvar), dir: &Path, tx: &Sender<Done>, ctx: &egui::Context) {
    let _com = Com::init();
    let wic = match Wic::new() {
        Ok(w) => w,
        Err(e) => return log(&format!("gallery: {e}")),
    };
    // made on the first recording, then shared by all of this worker's readers
    let mut gpu: Option<Option<Gpu>> = None;
    while let Some(task) = next(shared) {
        let done = match task {
            Task::Full(f) => {
                let fit = |w: u32, h: u32| fit_into(w, h, f.size);
                let image = wic.file(&f.path, fit).map(|(p, _)| p.image());
                if let Err(e) = &image {
                    log(&format!("gallery: {e}"));
                }
                Done::Full { id: f.id, image: image.ok() }
            }
            Task::Thumb(job) => {
                let made = cached(&wic, dir, job.key).map(Ok).unwrap_or_else(|| make(&wic, &mut gpu, dir, &job));
                finished(shared, job.id);
                match made {
                    Ok((pixels, meta)) => Done::Thumb { id: job.id, image: Some(pixels.image()), meta },
                    Err(e) => {
                        log(&format!("gallery: no thumbnail for {}: {e}", job.path.display()));
                        Done::Thumb { id: job.id, image: None, meta: Meta::default() }
                    }
                }
            }
        };
        if tx.send(done).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

/// The largest size of a `w` x `h` picture inside `size`, never bigger than it is.
fn fit_into(w: u32, h: u32, size: (u32, u32)) -> (u32, u32) {
    let scale = (size.0 as f64 / w.max(1) as f64).min(size.1 as f64 / h.max(1) as f64).min(1.0);
    (((w as f64 * scale).round() as u32).max(1), ((h as f64 * scale).round() as u32).max(1))
}

fn cached(wic: &Wic, dir: &Path, key: u64) -> Option<(Pixels, Meta)> {
    let bytes = std::fs::read(dir.join(thumb::file_name(key))).ok()?;
    let (meta, image) = thumb::unpack(&bytes)?;
    Some((wic.memory(image).ok()?, meta))
}

fn make(wic: &Wic, gpu: &mut Option<Option<Gpu>>, dir: &Path, job: &Job) -> Result<(Pixels, Meta), String> {
    let fit = |w, h| thumb::size_for(w, h, SHORT, LONG);
    let (pixels, meta) = match job.kind {
        Kind::Shot => {
            let (p, (width, height)) = wic.file(&job.path, fit)?;
            let alpha = p.alpha();
            (p, Meta { width, height, duration_ms: 0, alpha })
        }
        Kind::Gif => {
            let (p, (width, height)) = wic.file(&job.path, fit)?;
            let alpha = p.alpha();
            (p, Meta { width, height, duration_ms: play::gif_length(&job.path), alpha })
        }
        Kind::Recording => {
            let gpu = gpu.get_or_insert_with(|| Gpu::new().map_err(|e| log(&format!("gallery: thumbnails in software: {e}"))).ok());
            // a second in, or a tenth of a short one: past the first frame's fade, before the first seek point
            let at = |len: i64| (len / 10).min(SECOND);
            let even = |w, h| {
                let (w, h) = fit(w, h);
                ((w & !1).max(2), (h & !1).max(2))
            };
            let s = media::still(&job.path, gpu.as_ref(), at, even)?;
            let duration_ms = (s.duration / 10_000).clamp(0, u32::MAX as i64) as u32;
            let meta = Meta { width: s.native.0, height: s.native.1, duration_ms, alpha: false };
            (Pixels::opaque(s.size.0, s.size.1, s.bgrx), meta)
        }
    };
    store(dir, job.key, &pixels, meta);
    Ok((pixels, meta))
}

/// Writes the thumbnail next to its final name and renames it in, so no reader sees half of it.
fn store(dir: &Path, key: u64, pixels: &Pixels, meta: Meta) {
    let format = if meta.alpha { ImageFormat::Png } else { ImageFormat::Jpg };
    let encoded = match encode(&pixels.argb(), pixels.w, pixels.h, meta.alpha, format, 85) {
        Ok(e) => e,
        Err(e) => return log(&format!("gallery: can't encode a thumbnail: {e}")),
    };
    let file = dir.join(thumb::file_name(key));
    let tmp = file.with_extension("tmp");
    let written = std::fs::write(&tmp, thumb::pack(meta, &encoded)).and_then(|_| std::fs::rename(&tmp, &file));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        log(&format!("gallery: can't cache a thumbnail at {}: {e}", file.display()));
    }
}

/// Deletes cached thumbnails in `dir` of files that aren't there any more (or changed since).
pub(super) fn sweep(dir: &Path, live: HashSet<u64>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let names: Vec<String> = entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    let stale = thumb::stale(names.iter().map(String::as_str), &live);
    for name in &stale {
        let _ = std::fs::remove_file(dir.join(name));
    }
    if !stale.is_empty() {
        log(&format!("gallery: swept {} old thumbnails", stale.len()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_fit_their_box_without_growing() {
        assert_eq!(fit_into(2560, 1440, (1280, 1280)), (1280, 720));
        assert_eq!(fit_into(800, 2000, (1000, 500)), (200, 500));
        assert_eq!(fit_into(300, 200, (1000, 1000)), (300, 200));
    }

    #[test]
    fn thumbnails_are_cached_and_a_changed_file_misses() {
        let _com = Com::init();
        let wic = Wic::new().unwrap();
        let dir = std::env::temp_dir().join(format!("glint-thumbs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("shot.png");
        let (w, h) = (1200u32, 600u32);
        let px: Vec<u32> = (0..w * h).map(|i| 0xFF00_0000 | (((i % w) * 255 / w) << 16) | ((i / w) * 255 / h)).collect();
        std::fs::write(&path, encode(&px, w, h, false, ImageFormat::Png, 100).unwrap()).unwrap();
        let stat = |p: &Path| {
            let m = std::fs::metadata(p).unwrap();
            (m.len(), m.modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64)
        };
        let (size, modified) = stat(&path);
        let job = Job { id: 1, key: thumb::key(&path, size, modified), path: path.clone(), kind: Kind::Shot };
        assert!(cached(&wic, &dir, job.key).is_none());
        let mut gpu = None;
        let (made, meta) = make(&wic, &mut gpu, &dir, &job).unwrap();
        assert_eq!((made.w, made.h), (560, 280));
        assert_eq!(meta, Meta { width: 1200, height: 600, duration_ms: 0, alpha: false });
        let (again, again_meta) = cached(&wic, &dir, job.key).expect("cached");
        assert_eq!(((again.w, again.h), again_meta), ((560, 280), meta));
        // the jpeg in the cache keeps the picture: a corner and the middle come back close
        let at = |p: &Pixels, x: u32, y: u32| p.bgra[((y * p.w + x) * 4) as usize..][..3].to_vec();
        for (x, y) in [(5, 5), (280, 140), (550, 270)] {
            let (a, b) = (at(&made, x, y), at(&again, x, y));
            assert!(a.iter().zip(&b).all(|(a, b)| a.abs_diff(*b) < 12), "{x},{y}: {a:?} vs {b:?}");
        }
        // rewritten: a new key, so the old thumbnail isn't used (and gets swept)
        std::fs::write(&path, encode(&px[..w as usize * 300], w, 300, false, ImageFormat::Png, 100).unwrap()).unwrap();
        let (size2, modified2) = stat(&path);
        let key2 = thumb::key(&path, size2, modified2);
        assert_ne!(key2, job.key);
        assert!(cached(&wic, &dir, key2).is_none());
        let names: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        let live: HashSet<u64> = [key2].into_iter().collect();
        assert_eq!(thumb::stale(names.iter().map(String::as_str), &live), [thumb::file_name(job.key)]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
