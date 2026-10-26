//! How long recordings and gifs play, known without decoding them. The history says for what was
//! saved since it kept lengths; for the rest a look at the file says (an mp4's movie header, a
//! gif's frame delays), and what those looks found is cached by the thumbnail cache's keys, so
//! each file is looked at once.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::{Item, thumb};

const MAGIC: &[u8; 4] = b"gll1";
/// The cache's file, next to the thumbnails (which a sweep doesn't take for one).
pub const FILE: &str = "lengths.bin";

/// Lengths found by looking, by thumbnail key; 0 when the file didn't say.
#[derive(Debug, Default)]
pub struct Lengths {
    file: PathBuf,
    known: HashMap<u64, u32>,
    changed: bool,
}

impl Lengths {
    /// The cache in the thumbnail cache `dir`.
    pub fn load(dir: &Path) -> Lengths {
        let file = dir.join(FILE);
        let known = std::fs::read(&file).ok().map(|b| unpack(&b)).unwrap_or_default();
        Lengths { file, known, changed: false }
    }

    /// Fills in what's known of `items` without a length, from the cache; the indices of those
    /// that still need a look.
    pub fn fill(&self, items: &mut [Item]) -> Vec<usize> {
        let mut missing = Vec::new();
        for (i, item) in items.iter_mut().enumerate() {
            if !item.kind.moves() || item.duration_ms.is_some() {
                continue;
            }
            match self.known.get(&key(item)) {
                Some(&ms) => item.duration_ms = (ms > 0).then_some(ms),
                None => missing.push(i),
            }
        }
        missing
    }

    /// Looks at the `missing` items with `measure` and remembers what it says.
    pub fn measure(&mut self, items: &mut [Item], missing: &[usize], mut measure: impl FnMut(&Item) -> Option<u32>) {
        for &i in missing {
            let Some(item) = items.get_mut(i) else { continue };
            let ms = measure(item).filter(|ms| *ms > 0);
            item.duration_ms = ms;
            self.known.insert(key(item), ms.unwrap_or(0));
            self.changed = true;
        }
    }

    /// Writes the cache if it learned something, keeping only what `items` still are.
    pub fn store(&mut self, items: &[Item]) {
        if !std::mem::take(&mut self.changed) {
            return;
        }
        let live: HashSet<u64> = items.iter().map(key).collect();
        self.known.retain(|k, _| live.contains(k));
        if let Some(dir) = self.file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = self.file.with_extension("tmp");
        let written = std::fs::write(&tmp, pack(&self.known)).and_then(|_| std::fs::rename(&tmp, &self.file));
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            crate::log::log(&format!("gallery: can't keep lengths at {}: {e}", self.file.display()));
        }
    }
}

fn key(item: &Item) -> u64 {
    thumb::key(&item.path, item.size, item.modified)
}

fn pack(known: &HashMap<u64, u32>) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + known.len() * 12);
    out.extend_from_slice(MAGIC);
    for (k, ms) in known {
        out.extend_from_slice(&k.to_le_bytes());
        out.extend_from_slice(&ms.to_le_bytes());
    }
    out
}

fn unpack(bytes: &[u8]) -> HashMap<u64, u32> {
    let Some(body) = bytes.strip_prefix(MAGIC) else { return HashMap::new() };
    body.chunks_exact(12)
        .map(|c| (u64::from_le_bytes(c[..8].try_into().unwrap()), u32::from_le_bytes(c[8..].try_into().unwrap())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gallery::{Maker, OnDisk, Shelf, Source, collect};

    #[test]
    fn lengths_are_looked_up_once_and_kept_for_live_files_only() {
        let dir = std::env::temp_dir().join(format!("glint-lengths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let source = Source { maker: Maker::Glint, history: PathBuf::new(), root: dir.clone(), depth: 1, app_folder: None, accent: [0; 3] };
        let list = |names: &[&str]| {
            let found = names.iter().map(|n| OnDisk { path: dir.join(n), size: 10, modified: 5 }).collect();
            collect(vec![Shelf { source: &source, history: Vec::new(), found }], |_| None)
        };
        let mut items = list(&["a.mp4", "b.gif", "c.png", "broken.mp4"]);
        let mut cache = Lengths::load(&dir);
        let missing = cache.fill(&mut items);
        assert_eq!(missing.len(), 3, "screenshots don't play");
        let mut looked = Vec::new();
        cache.measure(&mut items, &missing, |i| {
            looked.push(i.name());
            match i.name().as_str() {
                "a.mp4" => Some(61_000),
                "b.gif" => Some(1_200),
                _ => None,
            }
        });
        looked.sort();
        assert_eq!(looked, ["a.mp4", "b.gif", "broken.mp4"]);
        cache.store(&items);
        let ms = |items: &[Item], n: &str| items.iter().find(|i| i.name() == n).unwrap().duration_ms;
        assert_eq!((ms(&items, "a.mp4"), ms(&items, "b.gif"), ms(&items, "broken.mp4")), (Some(61_000), Some(1_200), None));

        // the next start: nothing to look at again, not even what said nothing
        let mut again = list(&["a.mp4", "b.gif", "c.png", "broken.mp4"]);
        let cache = Lengths::load(&dir);
        assert!(cache.fill(&mut again).is_empty());
        assert_eq!((ms(&again, "a.mp4"), ms(&again, "broken.mp4")), (Some(61_000), None));
        // a.mp4 gone: once something new is learned it's dropped from the file
        let mut cache = cache;
        let mut fewer = list(&["b.gif", "d.mp4"]);
        let missing = cache.fill(&mut fewer);
        cache.measure(&mut fewer, &missing, |_| Some(5));
        cache.store(&fewer);
        assert_eq!(unpack(&std::fs::read(dir.join(FILE)).unwrap()).len(), 2);
        assert!(unpack(b"nope").is_empty() && unpack(MAGIC).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
