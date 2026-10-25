use super::clicks::Clicks;
use super::crop::Crop;
use super::info::Track;
use super::keys::Keys;
use super::math::Range;

const UNDO_LIMIT: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    pub start: i64,
    pub end: i64,
    pub removed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lane {
    pub source: Option<usize>,
    pub name: Option<String>,
    pub muted: bool,
    /// Percent of the track's own loudness, 0 to [`MAX_VOLUME`].
    pub volume: u16,
}

pub const EMPTY_TRACK: &str = "empty";
pub const FULL_VOLUME: u16 = 100;
pub const MAX_VOLUME: u16 = 200;

impl Lane {
    fn of(source: Option<usize>) -> Lane {
        Lane { source, name: None, muted: false, volume: FULL_VOLUME }
    }

    pub fn default_name<'a>(&self, tracks: &'a [Track]) -> &'a str {
        self.source.and_then(|k| tracks.get(k)).map_or(EMPTY_TRACK, |t| &t.name)
    }

    pub fn name<'a>(&'a self, tracks: &'a [Track]) -> &'a str {
        self.name.as_deref().unwrap_or_else(|| self.default_name(tracks))
    }

    /// The volume as a factor on the samples.
    pub fn gain(&self) -> f32 {
        gain(self.volume)
    }
}

/// `volume` percent as a factor on the samples.
pub fn gain(volume: u16) -> f32 {
    volume as f32 / FULL_VOLUME as f32
}

/// The timeline: the trimmed range, its pieces, markers, the sound lanes, and the crop.
#[derive(Clone, Debug, PartialEq)]
pub struct Edit {
    pub range: Range,
    pub segments: Vec<Segment>,
    pub markers: Vec<i64>,
    pub lanes: Vec<Lane>,
    pub crop: Option<Crop>,
    pub clicks: Option<Clicks>,
    pub keys: Option<Keys>,
}

impl Edit {
    pub fn new(duration: i64, tracks: usize) -> Self {
        let duration = duration.max(0);
        Edit {
            range: Range::full(duration),
            segments: vec![Segment { start: 0, end: duration, removed: false }],
            markers: Vec::new(),
            lanes: (0..tracks).map(|k| Lane::of(Some(k))).collect(),
            crop: None,
            clicks: None,
            keys: None,
        }
    }

    pub fn splits(&self) -> impl Iterator<Item = i64> + '_ {
        self.segments.iter().skip(1).map(|s| s.start)
    }

    pub fn segment_at(&self, t: i64) -> usize {
        self.segments.iter().position(|s| t < s.end).unwrap_or(self.segments.len() - 1)
    }

    pub fn split(&mut self, t: i64, min_len: i64) -> bool {
        let i = self.segment_at(t);
        let s = self.segments[i];
        if t - s.start < min_len || s.end - t < min_len {
            return false;
        }
        self.segments[i].end = t;
        self.segments.insert(i + 1, Segment { start: t, ..s });
        true
    }

    pub fn toggle_removed(&mut self, i: usize) -> bool {
        let Some(s) = self.segments.get(i).copied() else { return false };
        self.segments[i].removed = !s.removed;
        if self.kept_len() == 0 {
            self.segments[i].removed = s.removed;
            return false;
        }
        true
    }

    pub fn kept(&self) -> Vec<Range> {
        let mut out: Vec<Range> = Vec::new();
        for s in self.segments.iter().filter(|s| !s.removed) {
            let (start, end) = (s.start.max(self.range.start), s.end.min(self.range.end));
            if start >= end {
                continue;
            }
            match out.last_mut() {
                Some(last) if last.end == start => last.end = end,
                _ => out.push(Range { start, end }),
            }
        }
        out
    }

    pub(super) fn kept_len(&self) -> i64 {
        self.kept().iter().map(|r| r.len()).sum()
    }

    pub fn next_kept(&self, t: i64) -> Option<i64> {
        self.kept().into_iter().find(|r| t < r.end).map(|r| t.max(r.start))
    }

    pub(super) fn marker_near(&self, t: i64, within: i64) -> Option<usize> {
        let i = self.markers.iter().enumerate().min_by_key(|(_, m)| (*m - t).abs())?.0;
        ((self.markers[i] - t).abs() <= within).then_some(i)
    }

    pub fn add_marker(&mut self, t: i64, within: i64) -> bool {
        if self.marker_near(t, within).is_some() {
            return false;
        }
        let at = self.markers.partition_point(|&m| m < t);
        self.markers.insert(at, t);
        true
    }

    pub fn prev_marker(&self, t: i64, within: i64) -> Option<i64> {
        self.markers.iter().rev().copied().find(|&m| m < t - within)
    }

    pub fn next_marker(&self, t: i64, within: i64) -> Option<i64> {
        self.markers.iter().copied().find(|&m| m > t + within)
    }

    pub fn set_muted(&mut self, lane: usize, muted: bool) -> bool {
        let Some(l) = self.lanes.get_mut(lane) else { return false };
        let changed = l.muted != muted;
        l.muted = muted;
        changed
    }

    /// Sets a lane's volume, held to 0..=[`MAX_VOLUME`].
    pub fn set_volume(&mut self, lane: usize, volume: u16) -> bool {
        let Some(l) = self.lanes.get_mut(lane) else { return false };
        let volume = volume.min(MAX_VOLUME);
        let changed = l.volume != volume;
        l.volume = volume;
        changed
    }

    pub fn rename(&mut self, lane: usize, name: &str, tracks: &[Track]) -> bool {
        let Some(l) = self.lanes.get_mut(lane) else { return false };
        let name = name.trim();
        let custom = (!name.is_empty() && name != l.default_name(tracks)).then(|| name.to_string());
        let changed = l.name != custom;
        l.name = custom;
        changed
    }

    pub fn delete_lane(&mut self, lane: usize) -> bool {
        if lane >= self.lanes.len() {
            return false;
        }
        self.lanes.remove(lane);
        true
    }

    pub fn move_lane(&mut self, from: usize, to: usize) -> bool {
        if from >= self.lanes.len() || to >= self.lanes.len() || from == to {
            return false;
        }
        let l = self.lanes.remove(from);
        self.lanes.insert(to, l);
        true
    }

    pub fn add_empty(&mut self) -> usize {
        self.lanes.push(Lane::of(None));
        self.lanes.len() - 1
    }
}

pub fn source_time(kept: &[Range], out: i64) -> Option<(usize, i64)> {
    let mut base = 0;
    for (i, r) in kept.iter().enumerate() {
        if out < base + r.len() {
            return Some((i, r.start + (out - base).max(0)));
        }
        base += r.len();
    }
    None
}

pub fn snap_to(t: i64, targets: impl IntoIterator<Item = i64>, within: i64) -> i64 {
    targets.into_iter().filter(|&s| (s - t).abs() <= within).min_by_key(|&s| (s - t).abs()).unwrap_or(t)
}

pub struct Undo<T> {
    past: Vec<T>,
    future: Vec<T>,
}

impl<T> Default for Undo<T> {
    fn default() -> Self {
        Undo { past: Vec::new(), future: Vec::new() }
    }
}

impl<T: Clone + PartialEq> Undo<T> {
    pub fn record(&mut self, before: T, after: &T) {
        if before == *after {
            return;
        }
        if self.past.len() == UNDO_LIMIT {
            self.past.remove(0);
        }
        self.past.push(before);
        self.future.clear();
    }

    pub fn undo(&mut self, cur: &mut T) -> bool {
        let Some(prev) = self.past.pop() else { return false };
        self.future.push(std::mem::replace(cur, prev));
        true
    }

    pub fn redo(&mut self, cur: &mut T) -> bool {
        let Some(next) = self.future.pop() else { return false };
        self.past.push(std::mem::replace(cur, next));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: i64 = super::super::math::SECOND;

    fn r(start: i64, end: i64) -> Range {
        Range { start, end }
    }

    #[test]
    fn splitting_needs_room_on_both_sides() {
        let mut e = Edit::new(10 * S, 0);
        assert!(e.split(4 * S, S / 10));
        assert!(e.split(7 * S, S / 10));
        assert!(!e.split(7 * S, S / 10));
        assert!(!e.split(7 * S + 1, S / 10));
        assert!(!e.split(0, S / 10));
        assert!(!e.split(10 * S, S / 10));
        assert_eq!(e.splits().collect::<Vec<_>>(), [4 * S, 7 * S]);
        assert_eq!(e.segment_at(0), 0);
        assert_eq!(e.segment_at(4 * S - 1), 0);
        assert_eq!(e.segment_at(4 * S), 1);
        assert_eq!(e.segment_at(10 * S), 2);
    }

    #[test]
    fn deleted_segments_are_skipped_and_the_rest_joins() {
        let mut e = Edit::new(10 * S, 0);
        e.split(2 * S, 1);
        e.split(5 * S, 1);
        e.split(8 * S, 1);
        assert!(e.toggle_removed(1));
        assert_eq!(e.kept(), [r(0, 2 * S), r(5 * S, 10 * S)]);
        assert!(e.toggle_removed(2));
        assert_eq!(e.kept(), [r(0, 2 * S), r(8 * S, 10 * S)]);
        assert!(e.toggle_removed(2));
        assert_eq!(e.kept(), [r(0, 2 * S), r(5 * S, 10 * S)]);
        e.range = r(S, 9 * S);
        assert_eq!(e.kept(), [r(S, 2 * S), r(5 * S, 9 * S)]);
        assert_eq!(e.kept_len(), 5 * S);
        assert_eq!(e.next_kept(0), Some(S));
        assert_eq!(e.next_kept(3 * S), Some(5 * S));
        assert_eq!(e.next_kept(6 * S), Some(6 * S));
        assert_eq!(e.next_kept(9 * S), None);
    }

    #[test]
    fn the_last_kept_piece_cant_be_deleted() {
        let mut e = Edit::new(10 * S, 0);
        assert!(!e.toggle_removed(0));
        e.split(5 * S, 1);
        e.range = r(0, 5 * S);
        assert!(!e.toggle_removed(0));
        assert!(e.toggle_removed(1));
        assert_eq!(e.kept(), [r(0, 5 * S)]);
        assert!(!e.toggle_removed(9));
    }

    #[test]
    fn output_times_map_back_through_the_gaps() {
        let kept = [r(S, 2 * S), r(5 * S, 9 * S)];
        assert_eq!(source_time(&kept, 0), Some((0, S)));
        assert_eq!(source_time(&kept, S - 1), Some((0, 2 * S - 1)));
        assert_eq!(source_time(&kept, S), Some((1, 5 * S)));
        assert_eq!(source_time(&kept, 3 * S), Some((1, 7 * S)));
        assert_eq!(source_time(&kept, 5 * S), None);
    }

    #[test]
    fn snapping_picks_the_nearest_target_in_reach() {
        assert_eq!(snap_to(100, [90, 115, 300], 12), 90);
        assert_eq!(snap_to(104, [90, 115, 300], 12), 115);
        assert_eq!(snap_to(200, [90, 115, 300], 12), 200);
        assert_eq!(snap_to(200, [], 12), 200);
        assert_eq!(snap_to(288, [90, 115, 300], 12), 300);
        assert_eq!(snap_to(287, [90, 115, 300], 12), 287);
    }

    #[test]
    fn markers_stay_sorted_unique_and_navigate_past_the_playhead() {
        let mut e = Edit::new(10 * S, 0);
        let half = S / 120;
        assert!(e.add_marker(5 * S, half));
        assert!(e.add_marker(2 * S, half));
        assert!(e.add_marker(8 * S, half));
        assert!(!e.add_marker(5 * S + half, half));
        assert_eq!(e.markers, [2 * S, 5 * S, 8 * S]);
        assert_eq!(e.next_marker(0, half), Some(2 * S));
        assert_eq!(e.next_marker(5 * S, half), Some(8 * S));
        assert_eq!(e.next_marker(5 * S - 1, half), Some(8 * S));
        assert_eq!(e.next_marker(8 * S, half), None);
        assert_eq!(e.prev_marker(5 * S, half), Some(2 * S));
        assert_eq!(e.prev_marker(5 * S + 1, half), Some(2 * S));
        assert_eq!(e.prev_marker(2 * S, half), None);
        assert_eq!(e.prev_marker(10 * S, half), Some(8 * S));
        assert_eq!(e.marker_near(5 * S + 3, half), Some(1));
        assert_eq!(e.marker_near(6 * S, half), None);
    }

    #[test]
    fn undo_and_redo_walk_the_history() {
        let mut u = Undo::default();
        let mut e = Edit::new(10 * S, 0);
        let before = e.clone();
        e.split(5 * S, 1);
        u.record(before, &e);
        let before = e.clone();
        e.add_marker(S, 0);
        u.record(before, &e);
        let same = e.clone();
        u.record(same, &e);
        assert!(u.undo(&mut e));
        assert!(e.markers.is_empty());
        assert_eq!(e.segments.len(), 2);
        assert!(u.undo(&mut e));
        assert_eq!(e, Edit::new(10 * S, 0));
        assert!(!u.undo(&mut e));
        assert!(u.redo(&mut e));
        assert_eq!(e.segments.len(), 2);
        let before = e.clone();
        e.range = r(S, 9 * S);
        u.record(before, &e);
        assert!(!u.redo(&mut e));
        assert!(u.undo(&mut e));
        assert_eq!(e.range, r(0, 10 * S));
        for i in 0..UNDO_LIMIT + 10 {
            let before = e.clone();
            e.markers.push(i as i64);
            u.record(before, &e);
        }
        let mut n = 0;
        while u.undo(&mut e) {
            n += 1;
        }
        assert_eq!(n, UNDO_LIMIT);
    }

    #[test]
    fn lanes_rename_delete_reorder_and_grow_with_undo() {
        let tracks = [Track { stream: 1, name: "desktop".into() }, Track { stream: 2, name: "mic".into() }];
        let names = |e: &Edit| e.lanes.iter().map(|l| l.name(&tracks).to_string()).collect::<Vec<_>>();
        let mut u = Undo::default();
        let mut e = Edit::new(10 * S, 2);
        let fresh = e.clone();
        assert_eq!(names(&e), ["desktop", "mic"]);

        let before = e.clone();
        assert!(e.rename(1, "  voice ", &tracks));
        u.record(before, &e);
        assert_eq!(e.lanes[1].name.as_deref(), Some("voice"));
        assert!(!e.rename(1, "voice", &tracks));
        assert!(!e.rename(7, "x", &tracks));

        let before = e.clone();
        assert_eq!(e.add_empty(), 2);
        u.record(before, &e);
        assert_eq!(e.lanes[2], Lane { source: None, name: None, muted: false, volume: FULL_VOLUME });
        assert_eq!(names(&e), ["desktop", "voice", EMPTY_TRACK]);

        let before = e.clone();
        assert!(e.move_lane(2, 0));
        u.record(before, &e);
        assert!(!e.move_lane(0, 0) && !e.move_lane(0, 3) && !e.move_lane(5, 1));
        assert_eq!(names(&e), [EMPTY_TRACK, "desktop", "voice"]);

        let before = e.clone();
        assert!(e.delete_lane(1));
        u.record(before, &e);
        assert!(!e.delete_lane(2));
        assert_eq!(e.lanes.iter().map(|l| l.source).collect::<Vec<_>>(), [None, Some(1)]);

        let before = e.clone();
        assert!(e.set_muted(1, true) && !e.set_muted(1, true));
        u.record(before, &e);
        assert!(e.rename(1, "", &tracks) && e.lanes[1].name.is_none());
        assert!(e.rename(0, "silence", &tracks) && !e.rename(0, "silence", &tracks));
        assert!(e.rename(0, EMPTY_TRACK, &tracks) && e.lanes[0].name.is_none());

        let mut back = e.clone();
        let mut steps = 0;
        while u.undo(&mut back) {
            steps += 1;
        }
        assert_eq!((steps, &back), (5, &fresh));
        assert!(u.redo(&mut back) && u.redo(&mut back) && u.redo(&mut back));
        assert_eq!(names(&back), [EMPTY_TRACK, "desktop", "voice"]);
    }

    #[test]
    fn volumes_stay_with_their_lanes_and_undo() {
        let tracks = [Track { stream: 1, name: "desktop".into() }, Track { stream: 2, name: "mic".into() }];
        let mut u = Undo::default();
        let mut e = Edit::new(10 * S, 2);
        let fresh = e.clone();
        assert!(e.lanes.iter().all(|l| l.volume == FULL_VOLUME && l.gain() == 1.0));

        let before = e.clone();
        assert!(e.set_volume(0, 50) && !e.set_volume(0, 50) && !e.set_volume(9, 50));
        assert!(e.set_volume(1, 999) && e.lanes[1].volume == MAX_VOLUME);
        u.record(before, &e);
        assert_eq!((e.lanes[0].gain(), e.lanes[1].gain()), (0.5, 2.0));

        assert!(e.rename(0, "game", &tracks) && e.set_muted(0, true));
        assert!(e.move_lane(0, 1));
        assert_eq!(e.lanes.iter().map(|l| (l.source, l.volume)).collect::<Vec<_>>(), [(Some(1), MAX_VOLUME), (Some(0), 50)]);
        assert_eq!(e.lanes[1].name(&tracks), "game");
        assert!(!e.set_muted(1, true) && e.lanes[1].volume == 50, "muting keeps the volume");
        assert!(e.delete_lane(0));
        assert_eq!(e.add_empty(), 1);
        assert_eq!(e.lanes.iter().map(|l| (l.source, l.volume)).collect::<Vec<_>>(), [(Some(0), 50), (None, FULL_VOLUME)]);

        assert!(u.undo(&mut e) && e == fresh);
        assert!(u.redo(&mut e) && e.lanes[0].volume == 50);
        assert_eq!(gain(0), 0.0);
    }
}
