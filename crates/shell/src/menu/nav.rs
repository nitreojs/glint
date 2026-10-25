//! Which row is highlighted at each open level, and what keys and clicks do to that. Pure: the
//! popup mirrors `levels` with windows.

use super::{Item, Kind};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Key {
    Up,
    Down,
    Home,
    End,
    Left,
    Right,
    Enter,
    Esc,
    Char(char),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Effect {
    Nothing,
    /// The highlight moved.
    Moved,
    /// A submenu opened (one more level).
    Opened,
    /// The deepest submenu closed.
    Closed,
    Pick(usize),
    /// Close the whole menu without a pick.
    Dismiss,
}

/// The highlighted row of each open level: `levels[0]` is the menu, every next one the submenu
/// of the row highlighted before it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Nav {
    pub levels: Vec<Option<usize>>,
}

impl Nav {
    pub fn new(first: Option<usize>) -> Nav {
        Nav { levels: vec![first] }
    }

    /// The items shown at `level`.
    pub fn items<'a>(&self, root: &'a [Item], level: usize) -> &'a [Item] {
        let mut items = root;
        for &row in &self.levels[..level] {
            match row.and_then(|r| items.get(r)).and_then(Item::children) {
                Some(sub) => items = sub,
                None => return &[],
            }
        }
        items
    }

    fn deepest(&self) -> usize {
        self.levels.len() - 1
    }

    pub fn key(&mut self, root: &[Item], key: Key) -> Effect {
        let d = self.deepest();
        let items = self.items(root, d);
        let at = self.levels[d];
        let moved = |nav: &mut Nav, to: Option<usize>| {
            if to.is_some() && to != at {
                nav.levels[d] = to;
                Effect::Moved
            } else {
                Effect::Nothing
            }
        };
        match key {
            Key::Down => moved(self, step(items, at, true)),
            Key::Up => moved(self, step(items, at, false)),
            Key::Home => moved(self, step(items, None, true)),
            Key::End => moved(self, step(items, None, false)),
            Key::Right => self.open(root, d, true),
            Key::Left if d > 0 => self.close(),
            Key::Left => Effect::Nothing,
            Key::Esc if d > 0 => self.close(),
            Key::Esc => Effect::Dismiss,
            Key::Enter => match at {
                Some(row) => self.activate(root, d, row),
                None => Effect::Nothing,
            },
            Key::Char(c) => {
                let starts = |i: &Item| {
                    let first = i.label.chars().find(|c| c.is_alphanumeric());
                    i.selectable() && first.is_some_and(|f| f.to_lowercase().eq(c.to_lowercase()))
                };
                let matches: Vec<usize> = (0..items.len()).filter(|&i| starts(&items[i])).collect();
                match matches[..] {
                    [] => Effect::Nothing,
                    [only] => {
                        self.levels[d] = Some(only);
                        self.activate(root, d, only)
                    }
                    _ => {
                        let next = matches.iter().find(|&&i| at.is_none_or(|a| i > a)).unwrap_or(&matches[0]);
                        moved(self, Some(*next))
                    }
                }
            }
        }
    }

    /// A click on (or Enter at) `row` of `level`: picks a command, opens a submenu.
    pub fn activate(&mut self, root: &[Item], level: usize, row: usize) -> Effect {
        let Some(item) = self.items(root, level).get(row).filter(|i| i.selectable()) else { return Effect::Nothing };
        self.levels.truncate(level + 1);
        self.levels[level] = Some(row);
        match item.kind {
            Kind::Command(id) => Effect::Pick(id),
            Kind::Sub(_) => self.open(root, level, true),
            Kind::Separator => Effect::Nothing,
        }
    }

    /// Opens the submenu of the row highlighted at `level` (closing any deeper one), its first row
    /// highlighted when `select`.
    pub fn open(&mut self, root: &[Item], level: usize, select: bool) -> Effect {
        let row = self.levels[level];
        let Some(sub) = row.and_then(|r| self.items(root, level).get(r)).and_then(Item::children) else {
            return Effect::Nothing;
        };
        if self.levels.len() == level + 2 && !select {
            return Effect::Nothing; // already open
        }
        self.levels.truncate(level + 1);
        self.levels.push(if select { step(sub, None, true) } else { None });
        Effect::Opened
    }

    fn close(&mut self) -> Effect {
        self.levels.pop();
        Effect::Closed
    }
}

/// The next selectable row after `from` (or the first / last one), wrapping around.
pub(crate) fn step(items: &[Item], from: Option<usize>, down: bool) -> Option<usize> {
    let n = items.len();
    if n == 0 {
        return None;
    }
    let start = match (from, down) {
        (Some(f), true) => f + 1,
        (Some(f), false) => f + n - 1,
        (None, true) => 0,
        (None, false) => n - 1,
    };
    (0..n)
        .map(|k| if down { (start + k) % n } else { (start + n - k) % n })
        .find(|&i| items[i].selectable())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu() -> Vec<Item> {
        vec![
            Item::command(1, "save last 1:00"),
            Item::command(2, "pause").enabled(false),
            Item::separator(),
            Item::submenu("recent", vec![Item::command(10, "open"), Item::command(11, "show in folder")]),
            Item::command(3, "settings"),
            Item::command(4, "show log"),
            Item::separator(),
            Item::command(5, "quit"),
        ]
    }

    #[test]
    fn arrows_skip_greyed_rows_and_separators_and_wrap() {
        let m = menu();
        let mut nav = Nav::new(None);
        assert_eq!(nav.key(&m, Key::Down), Effect::Moved);
        assert_eq!(nav.levels, [Some(0)]);
        nav.key(&m, Key::Down);
        assert_eq!(nav.levels, [Some(3)], "pause is greyed, then a separator");
        nav.key(&m, Key::Up);
        assert_eq!(nav.levels, [Some(0)]);
        nav.key(&m, Key::Up);
        assert_eq!(nav.levels, [Some(7)], "up from the top wraps to quit");
        nav.key(&m, Key::Down);
        assert_eq!(nav.levels, [Some(0)]);
        nav.key(&m, Key::End);
        assert_eq!(nav.levels, [Some(7)]);
        nav.key(&m, Key::Home);
        assert_eq!(nav.levels, [Some(0)]);
        let mut fresh = Nav::new(None);
        fresh.key(&m, Key::Up);
        assert_eq!(fresh.levels, [Some(7)], "up with nothing highlighted starts at the bottom");
    }

    #[test]
    fn enter_picks_and_does_nothing_without_a_highlight() {
        let m = menu();
        let mut nav = Nav::new(None);
        assert_eq!(nav.key(&m, Key::Enter), Effect::Nothing);
        nav.levels[0] = Some(4);
        assert_eq!(nav.key(&m, Key::Enter), Effect::Pick(3));
    }

    #[test]
    fn right_and_enter_open_a_submenu_left_and_esc_close_it() {
        let m = menu();
        let mut nav = Nav::new(Some(3));
        assert_eq!(nav.key(&m, Key::Right), Effect::Opened);
        assert_eq!(nav.levels, [Some(3), Some(0)]);
        assert_eq!(nav.items(&m, 1).len(), 2);
        nav.key(&m, Key::Down);
        assert_eq!(nav.key(&m, Key::Enter), Effect::Pick(11));
        assert_eq!(nav.key(&m, Key::Left), Effect::Closed);
        assert_eq!(nav.levels, [Some(3)]);
        assert_eq!(nav.key(&m, Key::Left), Effect::Nothing);
        assert_eq!(nav.key(&m, Key::Enter), Effect::Opened);
        assert_eq!(nav.key(&m, Key::Esc), Effect::Closed);
        assert_eq!(nav.key(&m, Key::Esc), Effect::Dismiss);
        nav.levels[0] = Some(0);
        assert_eq!(nav.key(&m, Key::Right), Effect::Nothing, "a command has no submenu");
    }

    #[test]
    fn a_unique_first_letter_picks_a_shared_one_cycles() {
        let m = menu();
        let mut nav = Nav::new(None);
        assert_eq!(nav.key(&m, Key::Char('Q')), Effect::Pick(5));
        let mut nav = Nav::new(None);
        assert_eq!(nav.key(&m, Key::Char('s')), Effect::Moved);
        assert_eq!(nav.levels, [Some(0)]);
        nav.key(&m, Key::Char('s'));
        assert_eq!(nav.levels, [Some(4)]);
        nav.key(&m, Key::Char('s'));
        assert_eq!(nav.levels, [Some(5)]);
        nav.key(&m, Key::Char('s'));
        assert_eq!(nav.levels, [Some(0)], "wraps round");
        assert_eq!(nav.key(&m, Key::Char('p')), Effect::Nothing, "pause is greyed");
        assert_eq!(nav.key(&m, Key::Char('r')), Effect::Opened, "the one submenu opens");
        assert_eq!(nav.levels, [Some(3), Some(0)]);
        assert_eq!(nav.key(&m, Key::Char('o')), Effect::Pick(10), "letters work inside it");
    }

    #[test]
    fn a_hovered_submenu_opens_without_a_highlight_and_a_pick_closes_it() {
        let m = menu();
        let mut nav = Nav::new(Some(3));
        assert_eq!(nav.open(&m, 0, false), Effect::Opened);
        assert_eq!(nav.levels, [Some(3), None], "opened by hovering: nothing highlighted inside");
        assert_eq!(nav.open(&m, 0, false), Effect::Nothing);
        assert_eq!(nav.activate(&m, 0, 4), Effect::Pick(3));
        assert_eq!(nav.levels, [Some(4)], "picking at the top closes the submenu");
        assert_eq!(nav.activate(&m, 0, 2), Effect::Nothing);
    }

    #[test]
    fn an_empty_submenu_is_greyed() {
        let m = vec![Item::submenu("recent", vec![]), Item::command(1, "quit")];
        assert!(!m[0].selectable());
        let mut nav = Nav::new(None);
        nav.key(&m, Key::Down);
        assert_eq!(nav.levels, [Some(1)]);
    }
}
