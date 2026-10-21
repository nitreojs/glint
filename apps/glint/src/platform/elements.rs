//! What's under the cursor inside a window, for picking regions: the window's UI Automation
//! elements (what Chromium/Electron, Qt, WPF and UWP apps expose), its child windows and its
//! client area. Fetched on a worker thread because UIA calls go to the target app and can stall.

mod tree;
mod uia;

use std::collections::HashSet;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, TRUE, WPARAM};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetCursorPos, GetWindowRect, IsWindowVisible, PostMessageW,
};
use windows::core::BOOL;

use glint_core::geom::rect::Rect;
use glint_media::rect::WinRect;

use self::tree::Tree;
use self::uia::{Element, Failed, Uia};
use super::windows::client_rect;

/// Longest a request goes without an answer: past it the chain found so far is published and
/// the search goes on.
const BUDGET: Duration = Duration::from_millis(150);
/// A UIA call slower than this is a strike against its window; [`STRIKES`] of them and the
/// window gets no more UIA calls (a timeout counts as all of them).
const SLOW: Duration = Duration::from_millis(300);
const STRIKES: u8 = 3;
/// Background fetching stops once a window has this many elements.
const MAX_NODES: usize = 2000;
/// Least time between notifications about background progress.
const NOTIFY_EVERY: Duration = Duration::from_millis(100);

/// The element finder for one picker session. See [`Elements::start`].
pub(crate) struct Elements {
    sh: Arc<Shared>,
}

impl Elements {
    /// Starts the worker over `windows` (the picker's top-level snapshot, topmost first). It posts
    /// `msg` (wparam 0, lparam 0) to `notify` whenever [`latest`](Self::latest) or
    /// [`known`](Self::known) changed; a null `notify` gets no messages. Meanwhile it fetches the
    /// window under the cursor.
    pub(crate) fn start(windows: Vec<(HWND, Rect)>, notify: HWND, msg: u32) -> Result<Elements, String> {
        let sh = Arc::new(Shared {
            st: Mutex::new(State { uia: true, ..State::default() }),
            cv: Condvar::new(),
            notify: notify.0 as isize,
            msg,
        });
        let windows: Vec<(isize, Rect)> = windows.into_iter().map(|(h, r)| (h.0 as isize, r)).collect();
        let worker = sh.clone();
        std::thread::Builder::new()
            .name("elements".into())
            .spawn(move || run(&worker, windows))
            .map_err(|e| format!("failed to spawn the elements thread: {e}"))?;
        Ok(Elements { sh })
    }

    /// Asks for the chain at desktop point `pt`. Never waits on the worker: what's already known
    /// about `pt` is in [`latest`](Self::latest) on return, deeper elements follow with `msg`.
    pub(crate) fn request(&self, pt: (i32, i32)) {
        let mut st = self.sh.lock();
        if st.asked == Some(pt) {
            return;
        }
        st.asked = Some(pt);
        if st.ready {
            let (chain, more) = st.answer(pt);
            st.latest = Some((pt, chain));
            st.pending = more.then_some(pt);
        } else {
            st.pending = Some(pt);
        }
        if st.pending.is_some() {
            self.sh.cv.notify_one();
        }
    }

    /// The newest answer: the point it's for and the rects containing it, innermost first, ending
    /// with the window's client rect (if the point is in it) and the window rect; desktop px,
    /// clipped to the window, each enclosing the previous. Empty if no window is there.
    pub(crate) fn latest(&self) -> Option<((i32, i32), Vec<Rect>)> {
        self.sh.lock().latest.clone()
    }

    /// Every distinct rect found so far (windows, client areas, child windows, elements) with the
    /// index of its window in the snapshot, clipped to the window, except those entirely behind a
    /// window above; desktop px.
    pub(crate) fn known(&self) -> Vec<(usize, Rect)> {
        self.sh.lock().known.clone()
    }
}

impl Drop for Elements {
    /// Tells the worker to stop without waiting for it: it may be inside a UIA call, which ends
    /// within the UIA timeouts.
    fn drop(&mut self) {
        self.sh.lock().stop = true;
        self.sh.cv.notify_all();
    }
}

struct Shared {
    st: Mutex<State>,
    cv: Condvar,
    notify: isize,
    msg: u32,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn post(&self) {
        if self.notify != 0 {
            let _ = unsafe { PostMessageW(Some(HWND(self.notify as _)), self.msg, WPARAM(0), LPARAM(0)) };
        }
    }
}

/// One top-level window of the snapshot.
struct Win {
    hwnd: isize,
    rect: Rect,
    client: Option<Rect>,
    /// Visible descendant windows, clipped.
    kids: Vec<Rect>,
    /// None until UIA has been asked.
    tree: Option<Tree>,
    /// UIA gave up on it (no provider, or too slow).
    dead: bool,
    strikes: u8,
}

impl Win {
    fn new(hwnd: isize, rect: Rect) -> Win {
        unsafe extern "system" fn add(child: HWND, lp: LPARAM) -> BOOL {
            let list = unsafe { &mut *(lp.0 as *mut Vec<Rect>) };
            let mut rc = RECT::default();
            if unsafe { IsWindowVisible(child).as_bool() && GetWindowRect(child, &mut rc).is_ok() } {
                list.push(Rect::from_win(&rc));
            }
            TRUE
        }
        let h = HWND(hwnd as _);
        let mut all: Vec<Rect> = Vec::new();
        unsafe {
            let _ = EnumChildWindows(Some(h), Some(add), LPARAM(&mut all as *mut Vec<Rect> as isize));
        }
        let kids = all.iter().filter_map(|r| r.intersect(&rect)).collect();
        let client = client_rect(h).and_then(|c| c.intersect(&rect));
        Win { hwnd, rect, client, kids, tree: None, dead: false, strikes: 0 }
    }

    /// What UIA should fetch next for `pt`: None for the window's own element, Some(node) for a
    /// node's children; Err if nothing (all known, or UIA gave up).
    fn next_for(&self, pt: (i32, i32)) -> Result<Option<usize>, ()> {
        if self.dead {
            return Err(());
        }
        match &self.tree {
            None => Ok(None),
            Some(t) => t.frontier(pt).map(Some).ok_or(()),
        }
    }
}

#[derive(Default)]
struct State {
    stop: bool,
    /// `wins` is filled in.
    ready: bool,
    /// UIA is usable at all.
    uia: bool,
    wins: Vec<Win>,
    /// The point of the last request.
    asked: Option<(i32, i32)>,
    /// A point the worker still has to go deeper for.
    pending: Option<(i32, i32)>,
    latest: Option<((i32, i32), Vec<Rect>)>,
    known: Vec<(usize, Rect)>,
    seen: HashSet<(i32, i32, i32, i32)>,
    /// `known` grew since the last notification.
    dirty: bool,
}

impl State {
    fn window_at(&self, pt: (i32, i32)) -> Option<usize> {
        self.wins.iter().position(|w| w.rect.contains(pt.0, pt.1))
    }

    /// The chain at `pt` from what's known, and whether UIA could still find more.
    fn answer(&self, pt: (i32, i32)) -> (Vec<Rect>, bool) {
        let Some(w) = self.window_at(pt).map(|i| &self.wins[i]) else {
            return (Vec::new(), false);
        };
        let path = w.tree.as_ref().map(|t| t.walk(pt).into_iter().map(|n| t.nodes[n].rect)).into_iter().flatten();
        let chain = tree::chain(pt, w.rect, w.kids.iter().copied().chain(w.client).chain(path));
        (chain, self.uia && w.next_for(pt).is_ok())
    }

    /// Adds `r` of window `win` to `known` unless it's there already or hidden by a window above.
    fn remember(&mut self, win: usize, r: Rect) {
        let behind = self.wins[..win].iter().any(|w| w.rect.intersect(&r) == Some(r));
        if !behind && self.seen.insert((r.x, r.y, r.w, r.h)) {
            self.known.push((win, r));
            self.dirty = true;
        }
    }
}

fn run(sh: &Shared, windows: Vec<(isize, Rect)>) {
    let wins: Vec<Win> = windows.into_iter().map(|(h, r)| Win::new(h, r)).collect();
    {
        let mut st = sh.lock();
        st.wins = wins;
        st.ready = true;
        for i in 0..st.wins.len() {
            let w = &st.wins[i];
            let rects: Vec<Rect> = [w.rect].into_iter().chain(w.client).chain(w.kids.iter().copied()).collect();
            for r in rects {
                st.remember(i, r);
            }
        }
        if let Some(pt) = st.asked {
            let (chain, more) = st.answer(pt);
            st.latest = Some((pt, chain));
            st.pending = more.then_some(pt);
        }
        st.dirty = false;
    }
    sh.post();
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    match Uia::new() {
        Some(uia) => {
            let mut cursor = POINT::default();
            let warm = unsafe { GetCursorPos(&mut cursor) }.is_ok().then_some((cursor.x, cursor.y));
            let n = sh.lock().wins.len();
            Worker { sh, uia, handles: (0..n).map(|_| Vec::new()).collect(), focus: None, posted: Instant::now() }
                .run(warm);
        }
        None => {
            let mut st = sh.lock();
            st.uia = false;
            st.pending = None;
        }
    }
    if com {
        unsafe { CoUninitialize() };
    }
}

enum Job {
    /// Go deeper at a requested point, publishing as it goes.
    Ask((i32, i32)),
    /// Go deeper at a point nobody asked for yet.
    Warm((i32, i32)),
    /// Fetch a node's children in a window, for [`Elements::known`].
    Explore(usize, usize),
}

struct Worker<'a> {
    sh: &'a Shared,
    uia: Uia,
    /// Per window, the UIA element of each tree node.
    handles: Vec<Vec<Option<Element>>>,
    /// The window to explore when idle: the one last asked about.
    focus: Option<usize>,
    posted: Instant,
}

impl Worker<'_> {
    fn run(&mut self, mut warm: Option<(i32, i32)>) {
        loop {
            let job = {
                let mut st = self.sh.lock();
                loop {
                    if st.stop {
                        return;
                    }
                    if let Some(pt) = st.pending.take() {
                        break Job::Ask(pt);
                    }
                    if let Some(pt) = warm.take() {
                        break Job::Warm(pt);
                    }
                    if let Some(i) = self.focus
                        && !st.wins[i].dead
                        && let Some(t) = st.wins[i].tree.as_mut()
                        && t.nodes.len() < MAX_NODES
                        && let Some(n) = t.unexplored()
                    {
                        break Job::Explore(i, n);
                    }
                    if st.dirty {
                        st.dirty = false;
                        self.sh.post();
                    }
                    st = self.sh.cv.wait(st).unwrap_or_else(|e| e.into_inner());
                }
            };
            match job {
                Job::Ask(pt) => self.deepen(pt, true),
                Job::Warm(pt) => self.deepen(pt, false),
                Job::Explore(i, n) => {
                    self.fetch(i, Some(n));
                    let mut st = self.sh.lock();
                    if st.dirty && self.posted.elapsed() >= NOTIFY_EVERY {
                        st.dirty = false;
                        self.posted = Instant::now();
                        self.sh.post();
                    }
                }
            }
        }
    }

    /// Fetches down towards `pt` until its chain is complete or a newer request arrives; when
    /// `publish`, answers it at least every [`BUDGET`].
    fn deepen(&mut self, pt: (i32, i32), publish: bool) {
        let mut due = Instant::now() + BUDGET;
        loop {
            let next = {
                let st = self.sh.lock();
                if st.stop || st.pending.is_some() {
                    return;
                }
                st.window_at(pt).and_then(|i| {
                    self.focus = Some(i);
                    st.wins[i].next_for(pt).ok().map(|node| (i, node))
                })
            };
            let Some((i, node)) = next else { break };
            if publish && Instant::now() >= due {
                self.publish(pt);
                due = Instant::now() + BUDGET;
            }
            self.fetch(i, node);
        }
        if publish {
            self.publish(pt);
        }
    }

    /// Answers `pt` if it's still the latest request and the answer changed.
    fn publish(&mut self, pt: (i32, i32)) {
        let mut st = self.sh.lock();
        if st.asked != Some(pt) {
            return;
        }
        let (chain, _) = st.answer(pt);
        let changed = st.latest.as_ref().is_none_or(|(p, c)| *p != pt || *c != chain);
        if changed {
            st.latest = Some((pt, chain));
        }
        if changed || st.dirty {
            st.dirty = false;
            self.posted = Instant::now();
            self.sh.post();
        }
    }

    /// One UIA round trip for window `i`: its own element (`node` None) or a node's children.
    fn fetch(&mut self, i: usize, node: Option<usize>) {
        let hwnd = HWND(self.sh.lock().wins[i].hwnd as _);
        let t0 = Instant::now();
        let got = match node {
            None => self.uia.root(hwnd).map(|(el, r, kids)| (Some((el, r)), kids)),
            Some(n) => match &self.handles[i][n] {
                Some(el) => self.uia.kids(el).map(|kids| (None, kids)),
                None => Ok((None, Vec::new())),
            },
        };
        let slow = t0.elapsed() > SLOW;
        let mut st = self.sh.lock();
        let w = &mut st.wins[i];
        w.strikes += slow as u8;
        let added = match got {
            Err(Failed::Timeout) => {
                w.dead = true;
                Vec::new()
            }
            Err(Failed::Gone) => match (node, w.tree.as_mut()) {
                (Some(n), Some(t)) => {
                    t.add_kids(n, &[]);
                    Vec::new()
                }
                _ => {
                    w.dead = true;
                    Vec::new()
                }
            },
            Ok((root, kids)) => {
                let handles = &mut self.handles[i];
                if let Some((el, r)) = root {
                    w.tree = Some(Tree::new(r.intersect(&w.rect).unwrap_or(w.rect)));
                    *handles = vec![Some(el)];
                }
                let t = w.tree.as_mut().expect("the root is fetched first");
                let at = t.nodes.len();
                let rects: Vec<Rect> = kids.iter().map(|k| k.0).collect();
                let mut kids: Vec<Option<Element>> = kids.into_iter().map(|k| Some(k.1)).collect();
                for k in t.add_kids(node.unwrap_or(0), &rects) {
                    handles.push(kids[k].take());
                }
                t.nodes[at..].iter().map(|n| n.rect).collect()
            }
        };
        if w.strikes >= STRIKES {
            w.dead = true;
        }
        for r in added {
            st.remember(i, r);
        }
    }
}
