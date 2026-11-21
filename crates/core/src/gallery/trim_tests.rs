use std::path::{Path, PathBuf};

use super::{Counts, Date, Filters, Index, Narrow, Token, apply};
use crate::gallery::{Maker, OnDisk, Shelf, Source, collect};
use crate::history::{Entry, Kind};

#[test]
fn video_edit_filter_matches_named_and_recorded_exports_but_not_other_media() {
    let source = Source {
        maker: Maker::Glint,
        history: PathBuf::new(),
        root: PathBuf::new(),
        depth: 1,
        app_folder: None,
        accent: [0; 3],
    };
    let export = Entry { trimmed: Some(true), ..Entry::new(Kind::Recording, Some(Path::new("renamed.mp4")), 1) };
    let found = ["original.mp4", "old_TRIM_2.mp4", "old_trim.png", "old_trim.gif"]
        .into_iter()
        .map(|name| OnDisk { path: name.into(), size: 1, modified: 0 })
        .collect();
    let items = collect(vec![Shelf { source: &source, history: vec![export], found }], |_| Some((1, 1)));
    let index = Index::new(&items, |_| None);
    let today = Date { year: 2026, month: 11, day: 21 };

    let filtered = |trimmed| {
        let filters = Filters::default().with(&[Token::Trimmed(trimmed)]);
        let mut shown = Vec::new();
        let mut counts = Counts::default();
        apply(&index, &Narrow::new(&filters, &index, today), |_| true, &mut shown, &mut counts);
        let mut names: Vec<_> = shown.into_iter().map(|index| items[index].name()).collect();
        names.sort();

        (names, counts.trimmed)
    };
    let (untrimmed, counts) = filtered(false);
    let (trimmed, _) = filtered(true);

    assert_eq!(untrimmed, ["original.mp4"]);
    assert_eq!(trimmed, ["old_TRIM_2.mp4", "renamed.mp4"]);
    assert_eq!(counts, [1, 2]);
}
