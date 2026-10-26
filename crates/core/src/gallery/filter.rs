//! What the gallery is narrowed to besides the search words: a kind, a span of days, a length, the
//! app a capture shows and the app that made it. The search box names them too: whole phrases
//! ("today", "> 1 min", "afterglow") narrow as they're typed, and [`suggest`] offers what a half
//! typed one could become, so a pick turns into a chip.

use super::{Date, Filter, Item, Kind, Maker};

/// A span of days.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum When {
    Today,
    Yesterday,
    /// Since monday.
    Week,
    /// Since the first of the month.
    Month,
    /// A calendar month.
    InMonth { year: u16, month: u16 },
    /// From one day to another, both in.
    Days(Date, Date),
}

pub const MONTHS: [&str; 12] =
    ["january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"];

impl When {
    /// The presets the filter offers, in order.
    pub const PRESETS: [When; 4] = [When::Today, When::Yesterday, When::Week, When::Month];

    /// The first and the last day it covers, as day numbers, `today` being what it is.
    pub fn span(self, today: Date) -> (i64, i64) {
        let t = today.number();
        match self {
            When::Today => (t, t),
            When::Yesterday => (t - 1, t - 1),
            When::Week => (t - today.weekday(), t),
            When::Month => (t - (today.day as i64 - 1), t),
            When::InMonth { year, month } => {
                let first = Date { year, month, day: 1 };
                (first.number(), first.number() + first.days_in_month() as i64 - 1)
            }
            When::Days(a, b) => (a.number().min(b.number()), a.number().max(b.number())),
        }
    }

    pub fn label(self, today: Date) -> String {
        let short = |d: Date| format!("{} {}", crate::history::month(d.month).to_lowercase(), d.day);
        match self {
            When::Today => "today".into(),
            When::Yesterday => "yesterday".into(),
            When::Week => "this week".into(),
            When::Month => "this month".into(),
            When::InMonth { year, month } if year == today.year => MONTHS[(month - 1) as usize].into(),
            When::InMonth { year, month } => format!("{} {year}", crate::history::month(month).to_lowercase()),
            When::Days(a, b) if a == b => short(a),
            When::Days(a, b) => {
                let (a, b) = if a.number() <= b.number() { (a, b) } else { (b, a) };
                format!("{} – {}", short(a), short(b))
            }
        }
    }
}

/// How long something plays: at least `min_ms`, less than `max_ms`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Length {
    pub min_ms: u32,
    pub max_ms: Option<u32>,
}

impl Length {
    pub const SHORT: Length = Length { min_ms: 0, max_ms: Some(30_000) };
    pub const MEDIUM: Length = Length { min_ms: 30_000, max_ms: Some(120_000) };
    pub const LONG: Length = Length { min_ms: 120_000, max_ms: None };
    pub const PRESETS: [Length; 3] = [Length::SHORT, Length::MEDIUM, Length::LONG];

    pub fn admits(self, ms: u32) -> bool {
        ms >= self.min_ms && self.max_ms.is_none_or(|m| ms < m)
    }

    pub fn label(self) -> String {
        match (self.min_ms, self.max_ms) {
            (0, Some(max)) => format!("< {}", span_text(max)),
            (min, None) => format!("> {}", span_text(min)),
            (min, Some(max)) => format!("{} – {}", span_text(min), span_text(max)),
        }
    }
}

/// `45 s`, `2 min`, `1.5 min`, `2 h`.
fn span_text(ms: u32) -> String {
    let s = ms / 1000;
    if s < 60 || (s < 600 && !s.is_multiple_of(30)) {
        format!("{s} s")
    } else if s.is_multiple_of(3600) {
        format!("{} h", s / 3600)
    } else if s.is_multiple_of(60) {
        format!("{} min", s / 60)
    } else {
        format!("{:.1} min", s as f64 / 60.0)
    }
}

/// One thing the gallery can be narrowed by.
#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    Kind(Filter),
    When(When),
    Length(Length),
    /// An app's name, as the list shows it.
    App(String),
    Maker(Maker),
}

/// What a [`Token`] narrows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Facet {
    Kind,
    When,
    Length,
    App,
    Maker,
}

impl Token {
    pub fn facet(&self) -> Facet {
        match self {
            Token::Kind(_) => Facet::Kind,
            Token::When(_) => Facet::When,
            Token::Length(_) => Facet::Length,
            Token::App(_) => Facet::App,
            Token::Maker(_) => Facet::Maker,
        }
    }

    pub fn label(&self, today: Date) -> String {
        match self {
            Token::Kind(f) => kind_word(*f).into(),
            Token::When(w) => w.label(today),
            Token::Length(l) => l.label(),
            Token::App(a) => a.clone(),
            Token::Maker(m) => m.name().into(),
        }
    }
}

/// What the kind filter calls a kind.
pub fn kind_word(f: Filter) -> &'static str {
    match f {
        Filter::All => "all",
        Filter::Shots => "screenshots",
        Filter::Recordings => "recordings",
        Filter::Gifs => "gifs",
    }
}

/// Everything the gallery is narrowed to, besides the words.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filters {
    pub kind: Filter,
    pub when: Option<When>,
    pub length: Option<Length>,
    pub app: Option<String>,
    pub maker: Option<Maker>,
}

impl Filters {
    pub fn set(&mut self, t: Token) {
        match t {
            Token::Kind(k) => self.kind = k,
            Token::When(w) => self.when = Some(w),
            Token::Length(l) => self.length = Some(l),
            Token::App(a) => self.app = Some(a),
            Token::Maker(m) => self.maker = Some(m),
        }
    }

    pub fn clear(&mut self, f: Facet) {
        match f {
            Facet::Kind => self.kind = Filter::All,
            Facet::When => self.when = None,
            Facet::Length => self.length = None,
            Facet::App => self.app = None,
            Facet::Maker => self.maker = None,
        }
    }

    /// Whether `t` is what its facet is set to.
    pub fn has(&self, t: &Token) -> bool {
        match t {
            Token::Kind(k) => self.kind == *k,
            Token::When(w) => self.when == Some(*w),
            Token::Length(l) => self.length == Some(*l),
            Token::App(a) => self.app.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(a)),
            Token::Maker(m) => self.maker == Some(*m),
        }
    }

    /// What shows as chips, in the order they show (the kind has its own switch).
    pub fn chips(&self) -> Vec<Token> {
        let mut out = Vec::new();
        out.extend(self.when.map(Token::When));
        out.extend(self.length.map(Token::Length));
        out.extend(self.app.clone().map(Token::App));
        out.extend(self.maker.map(Token::Maker));
        out
    }

    /// `self` with the `tokens` the search box names whole on top.
    pub fn with(&self, tokens: &[Token]) -> Filters {
        let mut f = self.clone();
        for t in tokens {
            f.set(t.clone());
        }
        f
    }
}

/// An app among the captures.
#[derive(Clone, Debug, PartialEq)]
pub struct App {
    pub name: String,
    /// `name` lowercased.
    pub key: String,
    /// An exe of it, for its icon.
    pub exe: Option<String>,
    /// How many captures show it.
    pub total: usize,
}

/// What the filters look at in an item.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fact {
    pub kind: Kind,
    /// The local day it was made, as a day number.
    pub day: Option<i64>,
    pub ms: Option<u32>,
    /// Its app in [`Index::apps`].
    pub app: Option<u32>,
    pub maker: Maker,
}

/// The facts of a list, made once whenever the list changes, so narrowing it is a tight loop.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Index {
    pub facts: Vec<Fact>,
    /// Most captured first.
    pub apps: Vec<App>,
    /// The kinds and makers there are, in their own order.
    pub kinds: Vec<Kind>,
    pub makers: Vec<Maker>,
}

impl Index {
    /// `day` gives an item's local date.
    pub fn new(items: &[Item], day: impl Fn(usize) -> Option<Date>) -> Index {
        let mut apps: Vec<App> = Vec::new();
        let mut at = std::collections::HashMap::<String, usize>::new();
        let mut of_item = Vec::with_capacity(items.len());
        for it in items {
            let Some(name) = it.app.as_deref().map(str::trim).filter(|n| !n.is_empty()) else {
                of_item.push(None);
                continue;
            };
            let key = name.to_lowercase();
            let i = *at.entry(key.clone()).or_insert_with(|| {
                apps.push(App { name: name.to_string(), key, exe: None, total: 0 });
                apps.len() - 1
            });
            apps[i].total += 1;
            if apps[i].exe.is_none() {
                apps[i].exe.clone_from(&it.exe);
            }
            of_item.push(Some(i));
        }
        // most captured first; the indices move with them
        let mut order: Vec<usize> = (0..apps.len()).collect();
        order.sort_by(|&a, &b| apps[b].total.cmp(&apps[a].total).then_with(|| apps[a].key.cmp(&apps[b].key)));
        let mut rank = vec![0u32; apps.len()];
        for (r, &i) in order.iter().enumerate() {
            rank[i] = r as u32;
        }
        let apps = order.iter().map(|&i| apps[i].clone()).collect();
        let facts = items
            .iter()
            .zip(of_item)
            .enumerate()
            .map(|(i, (it, app))| Fact {
                kind: it.kind,
                day: day(i).map(Date::number),
                ms: it.duration_ms,
                app: app.map(|a| rank[a]),
                maker: it.maker,
            })
            .collect();
        let kinds = [Kind::Shot, Kind::Recording, Kind::Gif].into_iter().filter(|k| items.iter().any(|i| i.kind == *k)).collect();
        let makers = Maker::ALL.into_iter().filter(|m| items.iter().any(|i| i.maker == *m)).collect();
        Index { facts, apps, kinds, makers }
    }

    pub fn app(&self, name: &str) -> Option<u32> {
        self.apps.iter().position(|a| a.key.eq_ignore_ascii_case(name)).map(|i| i as u32)
    }
}

/// [`Filters`] made concrete for one list and one day.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Narrow {
    kind: Filter,
    days: Option<(i64, i64)>,
    length: Option<Length>,
    /// `Some(None)`: an app none of the items shows.
    app: Option<Option<u32>>,
    maker: Option<Maker>,
    today: i64,
    week: (i64, i64),
    month: (i64, i64),
}

impl Narrow {
    pub fn new(f: &Filters, index: &Index, today: Date) -> Narrow {
        Narrow {
            kind: f.kind,
            days: f.when.map(|w| w.span(today)),
            length: f.length,
            app: f.app.as_deref().map(|a| index.app(a)),
            maker: f.maker,
            today: today.number(),
            week: When::Week.span(today),
            month: When::Month.span(today),
        }
    }

    /// Which facets `f` passes, as bits in [`Facet`] order.
    fn passes(&self, f: &Fact) -> u8 {
        let within = |(a, b): (i64, i64)| f.day.is_some_and(|d| d >= a && d <= b);
        let bits = [
            self.kind.admits(f.kind),
            self.days.is_none_or(within),
            self.length.is_none_or(|l| f.ms.is_some_and(|ms| l.admits(ms))),
            self.app.is_none_or(|a| a.is_some() && a == f.app),
            self.maker.is_none_or(|m| m == f.maker),
        ];
        bits.iter().enumerate().fold(0, |acc, (i, b)| acc | (u8::from(*b) << i))
    }
}

const ALL_FACETS: u8 = 0b1_1111;

/// How many items each option would show, the other facets as they are.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Counts {
    /// All, screenshots, recordings, gifs.
    pub kinds: [usize; 4],
    /// [`When::PRESETS`].
    pub when: [usize; 4],
    /// [`Length::PRESETS`].
    pub length: [usize; 3],
    /// By [`Index::apps`].
    pub apps: Vec<usize>,
    /// By [`Maker::ALL`].
    pub makers: [usize; 2],
    /// By local day, the when facet left out: what the calendar marks.
    pub days: std::collections::HashMap<i64, usize>,
}

/// Puts the indices of the items that show in `shown`, in order, and counts each facet's options
/// into `counts`. `visible` says whether an item is in at all (it holds the search's words and
/// isn't being deleted).
pub fn apply(index: &Index, narrow: &Narrow, visible: impl Fn(usize) -> bool, shown: &mut Vec<usize>, counts: &mut Counts) {
    shown.clear();
    counts.kinds = [0; 4];
    counts.when = [0; 4];
    counts.length = [0; 3];
    counts.makers = [0; 2];
    counts.apps.clear();
    counts.apps.resize(index.apps.len(), 0);
    counts.days.clear();
    let but = |facet: Facet| ALL_FACETS & !(1 << facet as u8);
    for (i, f) in index.facts.iter().enumerate() {
        if !visible(i) {
            continue;
        }
        let p = narrow.passes(f);
        if p == ALL_FACETS {
            shown.push(i);
        }
        if p & but(Facet::Kind) == but(Facet::Kind) {
            counts.kinds[0] += 1;
            counts.kinds[match f.kind {
                Kind::Shot => 1,
                Kind::Recording => 2,
                Kind::Gif => 3,
            }] += 1;
        }
        if p & but(Facet::When) == but(Facet::When)
            && let Some(d) = f.day
        {
            let spans = [(narrow.today, narrow.today), (narrow.today - 1, narrow.today - 1), narrow.week, narrow.month];
            for (n, (a, b)) in counts.when.iter_mut().zip(spans) {
                *n += usize::from(d >= a && d <= b);
            }
            *counts.days.entry(d).or_default() += 1;
        }
        if p & but(Facet::Length) == but(Facet::Length)
            && let Some(ms) = f.ms
        {
            for (n, l) in counts.length.iter_mut().zip(Length::PRESETS) {
                *n += usize::from(l.admits(ms));
            }
        }
        if p & but(Facet::App) == but(Facet::App)
            && let Some(a) = f.app
        {
            counts.apps[a as usize] += 1;
        }
        if p & but(Facet::Maker) == but(Facet::Maker) {
            counts.makers[f.maker as usize] += 1;
        }
    }
}

/// How many of the `visible` items `narrow` lets through.
pub fn count(index: &Index, narrow: &Narrow, visible: impl Fn(usize) -> bool) -> usize {
    index.facts.iter().enumerate().filter(|(i, f)| visible(*i) && narrow.passes(f) == ALL_FACETS).count()
}

/// What the search box can name: the kinds, makers and apps there are.
pub struct Vocabulary<'a> {
    pub index: &'a Index,
    pub today: Date,
}

/// The words a kind goes by.
const KIND_WORDS: [(&str, Filter); 6] = [
    ("screenshots", Filter::Shots),
    ("shots", Filter::Shots),
    ("recordings", Filter::Recordings),
    ("videos", Filter::Recordings),
    ("clips", Filter::Recordings),
    ("gifs", Filter::Gifs),
];

impl Vocabulary<'_> {
    fn kind_named(&self, word: &str) -> Option<Filter> {
        let k = KIND_WORDS.iter().find(|(w, _)| *w == word || w.strip_suffix('s') == Some(word))?.1;
        (self.index.kinds.len() > 1 && self.index.kinds.iter().any(|kind| k.admits(*kind))).then_some(k)
    }

    fn maker_named(&self, word: &str) -> Option<Maker> {
        (self.index.makers.len() > 1).then(|| Maker::ALL.into_iter().find(|m| m.name() == word)).flatten()
    }

    /// The token `phrase` (lowercase) names whole, if any.
    fn whole(&self, phrase: &str) -> Option<Token> {
        if let Some(w) = when_named(phrase, self.today) {
            return Some(Token::When(w));
        }
        if let Some(l) = length_named(phrase) {
            return Some(Token::Length(l));
        }
        if let Some(m) = self.maker_named(phrase) {
            return Some(Token::Maker(m));
        }
        self.kind_named(phrase).map(Token::Kind)
    }
}

/// The span `phrase` names: today, yesterday, this week, this month, a month's name.
pub fn when_named(phrase: &str, today: Date) -> Option<When> {
    match phrase {
        "today" => Some(When::Today),
        "yesterday" => Some(When::Yesterday),
        "this week" | "week" => Some(When::Week),
        "this month" | "month" => Some(When::Month),
        _ => {
            // the whole name, its first three letters, or "sept"
            let named = |m: &str| m == phrase || m[..3] == *phrase || (m == "september" && phrase == "sept");
            let m = MONTHS.iter().position(|m| named(m))? as u16 + 1;
            // the latest such month that has begun
            let year = if m <= today.month { today.year } else { today.year - 1 };
            Some(When::InMonth { year, month: m })
        }
    }
}

/// The length `phrase` names: `> 1 min`, `<30s`, `1-2 min`, `30 s – 2 min`; a unit is needed.
pub fn length_named(phrase: &str) -> Option<Length> {
    let p: String = phrase.chars().filter(|c| !c.is_whitespace()).collect();
    let p = p.replace('–', "-");
    if let Some(rest) = p.strip_prefix('>') {
        return Some(Length { min_ms: amount(rest.trim_start_matches('='), None)?, max_ms: None });
    }
    if let Some(rest) = p.strip_prefix('<') {
        return Some(Length { min_ms: 0, max_ms: Some(amount(rest.trim_start_matches('='), None)?) });
    }
    let (a, b) = p.split_once('-')?;
    // `1-2 min`: the second's unit goes for both
    let unit = unit_of(b)?;
    let (lo, hi) = (amount(a, Some(unit))?, amount(b, None)?);
    (lo < hi).then_some(Length { min_ms: lo, max_ms: Some(hi) })
}

/// Milliseconds in one of a unit word.
fn unit_ms(word: &str) -> Option<u32> {
    match word {
        "s" | "sec" | "secs" | "second" | "seconds" => Some(1_000),
        "m" | "min" | "mins" | "minute" | "minutes" => Some(60_000),
        "h" | "hr" | "hour" | "hours" => Some(3_600_000),
        _ => None,
    }
}

/// `amount` split into its number and unit word.
fn split_number(text: &str) -> (&str, &str) {
    let end = text.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ',')).unwrap_or(text.len());
    (&text[..end], &text[end..])
}

fn unit_of(text: &str) -> Option<u32> {
    unit_ms(split_number(text).1)
}

/// `90s`, `1.5min`: milliseconds; with no unit, `unit`'s (none: no amount).
fn amount(text: &str, unit: Option<u32>) -> Option<u32> {
    let (number, word) = split_number(text);
    let n: f64 = number.replace(',', ".").parse().ok()?;
    let per = if word.is_empty() { unit? } else { unit_ms(word)? };
    let ms = n * per as f64;
    (ms.is_finite() && ms > 0.0 && ms < u32::MAX as f64).then_some(ms.round() as u32)
}

/// The search box read: the tokens its phrases name whole, and the words left to search with.
pub fn read(query: &str, vocab: &Vocabulary) -> (Vec<Token>, Vec<String>) {
    let lower = query.to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    let (mut tokens, mut rest) = (Vec::new(), Vec::new());
    let mut i = 0;
    while i < words.len() {
        // the longest phrase first: "this week" before "week"
        let found = (1..=3.min(words.len() - i)).rev().find_map(|n| vocab.whole(&words[i..i + n].join(" ")).map(|t| (t, n)));
        match found {
            Some((t, n)) => {
                tokens.push(t);
                i += n;
            }
            None => {
                rest.push(words[i].to_string());
                i += 1;
            }
        }
    }
    (tokens, rest)
}

/// Something the search box offers while a phrase is being typed.
#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub token: Token,
    /// Where in the query the words it stands for begin (bytes); picking it drops them.
    pub from: usize,
}

/// The most suggestions shown.
pub const MOST: usize = 7;

/// What the end of `query` could be: spans, lengths, kinds, makers, then apps (most captured
/// first); none for what `filters` already is.
pub fn suggest(query: &str, vocab: &Vocabulary, filters: &Filters) -> Vec<Suggestion> {
    let mut out: Vec<Suggestion> = Vec::new();
    let add = |out: &mut Vec<Suggestion>, token: Token, from: usize| {
        if out.len() < MOST && !filters.has(&token) && !out.iter().any(|s| s.token == token) {
            out.push(Suggestion { token, from });
        }
    };
    // where the last three words begin, the longest stretch first
    let starts: Vec<usize> = query.char_indices().filter(|&(i, c)| !c.is_whitespace() && (i == 0 || query[..i].ends_with(char::is_whitespace))).map(|(i, _)| i).collect();
    let starts = &starts[starts.len().saturating_sub(3)..];
    let mut apps: Vec<(usize, Suggestion)> = Vec::new();
    for &from in starts {
        let typed = query[from..].trim().to_lowercase();
        if typed.is_empty() {
            continue;
        }
        if let Some(t) = vocab.whole(&typed) {
            add(&mut out, t, from);
        }
        if typed.chars().count() >= 2 {
            for phrase in ["today", "yesterday", "this week", "this month"] {
                if phrase.starts_with(&typed) || phrase.split(' ').any(|w| w.len() > 2 && w.starts_with(&typed)) {
                    add(&mut out, Token::When(when_named(phrase, vocab.today).unwrap_or(When::Today)), from);
                }
            }
        }
        if typed.chars().count() >= 3 && !typed.contains(' ') {
            for m in MONTHS.iter().filter(|m| m.starts_with(&typed)) {
                if let Some(w) = when_named(m, vocab.today) {
                    add(&mut out, Token::When(w), from);
                }
            }
        }
        for l in lengths_from(&typed) {
            add(&mut out, Token::Length(l), from);
        }
        if typed.chars().count() >= 2 {
            for (word, l) in [("short", Length::SHORT), ("long", Length::LONG)] {
                if word.starts_with(&typed) {
                    add(&mut out, Token::Length(l), from);
                }
            }
            for m in Maker::ALL {
                if m.name().starts_with(&typed) && vocab.maker_named(m.name()).is_some() {
                    add(&mut out, Token::Maker(m), from);
                }
            }
        }
        if typed.chars().count() >= 3 {
            for (word, _) in KIND_WORDS {
                if word.starts_with(&typed)
                    && let Some(k) = vocab.kind_named(word)
                {
                    add(&mut out, Token::Kind(k), from);
                }
            }
        }
        // a bare number is a length being typed, not an app
        let apps_too = typed.chars().any(char::is_alphabetic);
        for (rank, app) in vocab.index.apps.iter().enumerate().filter(|_| apps_too) {
            let starts_word = app.key.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(&typed));
            if starts_word || (typed.chars().count() >= 3 && app.key.contains(&typed)) {
                // a word's start beats the middle of one, then the most captured
                let score = rank + if starts_word { 0 } else { 10_000 };
                if !apps.iter().any(|(_, s)| s.token == Token::App(app.name.clone())) {
                    apps.push((score, Suggestion { token: Token::App(app.name.clone()), from }));
                }
            }
        }
    }
    apps.sort_by_key(|(score, _)| *score);
    for (_, s) in apps {
        add(&mut out, s.token, s.from);
    }
    out
}

/// What a half-typed `> 2` or `<30 se` could be: with the unit typed, that; with none yet, seconds
/// and minutes both.
fn lengths_from(typed: &str) -> Vec<Length> {
    let p: String = typed.chars().filter(|c| !c.is_whitespace()).collect();
    let (op, rest) = match p.chars().next() {
        Some('>') => ('>', &p[1..]),
        Some('<') => ('<', &p[1..]),
        _ => return Vec::new(),
    };
    let rest = rest.trim_start_matches('=');
    let (number, word) = split_number(rest);
    let Ok(n) = number.replace(',', ".").parse::<f64>() else { return Vec::new() };
    let units: Vec<u32> = if word.is_empty() {
        vec![1_000, 60_000]
    } else {
        let names = [("seconds", 1_000), ("sec", 1_000), ("minutes", 60_000), ("min", 60_000), ("hours", 3_600_000)];
        let mut u: Vec<u32> = names.iter().filter(|(name, _)| name.starts_with(word)).map(|(_, ms)| *ms).collect();
        u.dedup();
        u
    };
    units
        .into_iter()
        .filter_map(|per| {
            let ms = n * per as f64;
            (ms > 0.0 && ms < u32::MAX as f64).then_some(ms.round() as u32)
        })
        .map(|ms| if op == '>' { Length { min_ms: ms, max_ms: None } } else { Length { min_ms: 0, max_ms: Some(ms) } })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::gallery::{OnDisk, Shelf, Source, collect};
    use crate::history::{Entry, Kind as HistoryKind};

    const TODAY: Date = Date { year: 2026, month: 10, day: 25 }; // a sunday

    /// Six captures; an item's time is its day's number, so its date comes straight from it.
    fn items() -> Vec<Item> {
        let root = std::env::temp_dir();
        let line = |name: &str, day: i64, app: Option<&str>, ms: Option<u32>| {
            let kind = if name.ends_with(".png") { HistoryKind::Shot } else { HistoryKind::Recording };
            Entry { time: day as u64, ..Entry::new(kind, Some(&root.join(name)), 1).with_app(app, Some("x.exe")).with_duration(ms) }
        };
        let tg = Some("Telegram Desktop");
        let cs = Some("Counter-Strike 2");
        let glint = vec![line("a.png", 20751, tg, None), line("b.mp4", 20750, tg, Some(20_000)), line("e.gif", 20700, None, Some(4_000))];
        let afterglow = vec![
            line("c.mp4", 20746, cs, Some(90_000)),
            line("d.mp4", 20730, cs, Some(200_000)),
            line("f.mp4", 20751, Some("Discord"), None),
        ];
        let source = |maker| Source { maker, history: PathBuf::new(), root: root.clone(), depth: 1, app_folder: None, accent: [0; 3] };
        let (g, a) = (source(Maker::Glint), source(Maker::Afterglow));
        let shelves = vec![
            Shelf { source: &g, history: glint, found: Vec::<OnDisk>::new() },
            Shelf { source: &a, history: afterglow, found: Vec::new() },
        ];
        collect(shelves, |_| Some((1, 1)))
    }

    fn index(items: &[Item]) -> Index {
        Index::new(items, |i| Some(Date::from_number(items[i].time as i64)))
    }

    /// What `f` shows, by name, sorted; and the counts.
    fn shown(items: &[Item], f: &Filters) -> (Vec<String>, Counts) {
        let index = index(items);
        let (mut out, mut counts) = (Vec::new(), Counts::default());
        apply(&index, &Narrow::new(f, &index, TODAY), |_| true, &mut out, &mut counts);
        let mut names: Vec<String> = out.iter().map(|&i| items[i].name()).collect();
        names.sort();
        (names, counts)
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_index_knows_apps_kinds_and_makers() {
        let items = items();
        let index = index(&items);
        let apps: Vec<(&str, usize)> = index.apps.iter().map(|a| (a.name.as_str(), a.total)).collect();
        assert_eq!(apps, [("Counter-Strike 2", 2), ("Telegram Desktop", 2), ("Discord", 1)]);
        assert_eq!(index.apps[0].exe.as_deref(), Some("x.exe"));
        assert_eq!(index.kinds, [Kind::Shot, Kind::Recording, Kind::Gif]);
        assert_eq!(index.makers, Maker::ALL);
        assert_eq!(index.app("telegram DESKTOP"), Some(1));
    }

    #[test]
    fn nothing_set_shows_everything_and_counts_every_option() {
        let items = items();
        let (all, c) = shown(&items, &Filters::default());
        assert_eq!(all.len(), 6);
        assert_eq!(c.kinds, [6, 1, 4, 1]);
        assert_eq!(c.when, [2, 1, 4, 5], "today, yesterday, this week, this month");
        assert_eq!(c.length, [2, 1, 1], "screenshots and unknown lengths are in none");
        assert_eq!(c.apps, [2, 2, 1]);
        assert_eq!(c.makers, [3, 3]);
        assert_eq!(c.days.get(&20751), Some(&2));
    }

    #[test]
    fn each_filter_narrows() {
        let items = items();
        let only = |f: Filters| shown(&items, &f).0;
        assert_eq!(only(Filters { kind: Filter::Recordings, ..Default::default() }), names(&["b.mp4", "c.mp4", "d.mp4", "f.mp4"]));
        assert_eq!(only(Filters { kind: Filter::Gifs, ..Default::default() }), names(&["e.gif"]));
        assert_eq!(only(Filters { when: Some(When::Today), ..Default::default() }), names(&["a.png", "f.mp4"]));
        assert_eq!(only(Filters { when: Some(When::Yesterday), ..Default::default() }), names(&["b.mp4"]));
        assert_eq!(only(Filters { when: Some(When::Week), ..Default::default() }), names(&["a.png", "b.mp4", "c.mp4", "f.mp4"]));
        assert_eq!(only(Filters { when: Some(When::Month), ..Default::default() }).len(), 5);
        assert_eq!(only(Filters { when: Some(When::InMonth { year: 2026, month: 9 }), ..Default::default() }), names(&["e.gif"]));
        let oct = |d| Date { year: 2026, month: 10, day: d };
        assert_eq!(only(Filters { when: Some(When::Days(oct(20), oct(4))), ..Default::default() }), names(&["c.mp4", "d.mp4"]));
        assert_eq!(only(Filters { length: Some(Length::SHORT), ..Default::default() }), names(&["b.mp4", "e.gif"]));
        assert_eq!(only(Filters { length: Some(Length::MEDIUM), ..Default::default() }), names(&["c.mp4"]));
        assert_eq!(only(Filters { length: Some(Length::LONG), ..Default::default() }), names(&["d.mp4"]));
        assert_eq!(only(Filters { length: length_named("> 1 min"), ..Default::default() }), names(&["c.mp4", "d.mp4"]));
        assert_eq!(only(Filters { app: Some("telegram desktop".into()), ..Default::default() }), names(&["a.png", "b.mp4"]));
        assert!(only(Filters { app: Some("Photoshop".into()), ..Default::default() }).is_empty(), "an app no capture shows");
        assert_eq!(only(Filters { maker: Some(Maker::Afterglow), ..Default::default() }), names(&["c.mp4", "d.mp4", "f.mp4"]));
        assert_eq!(only(Filters { maker: Some(Maker::Glint), ..Default::default() }), names(&["a.png", "b.mp4", "e.gif"]));
    }

    #[test]
    fn filters_combine_and_each_facet_counts_with_the_others_set() {
        let items = items();
        let f = Filters { kind: Filter::Recordings, when: Some(When::Week), maker: Some(Maker::Afterglow), ..Default::default() };
        let (all, c) = shown(&items, &f);
        assert_eq!(all, names(&["c.mp4", "f.mp4"]));
        // made by: recordings this week, by either app
        assert_eq!(c.makers, [1, 2]);
        // kinds: afterglow's this week, whatever kind
        assert_eq!(c.kinds, [2, 0, 2, 0]);
        // when: afterglow's recordings, any day
        assert_eq!(c.when, [1, 0, 2, 3]);
        assert_eq!(c.apps, [1, 0, 1]);
        // what's hidden (being deleted, or not holding the words) counts nowhere
        let index = index(&items);
        let (mut out, mut counts) = (Vec::new(), Counts::default());
        let f_at = items.iter().position(|i| i.name() == "f.mp4").unwrap();
        apply(&index, &Narrow::new(&f, &index, TODAY), |i| i != f_at, &mut out, &mut counts);
        assert_eq!((out.len(), counts.makers), (1, [1, 1]));
    }

    #[test]
    fn spans_and_lengths_read_and_label() {
        assert_eq!(When::Week.span(TODAY), (20745, 20751));
        assert_eq!(When::Month.span(TODAY), (20727, 20751));
        let feb = When::InMonth { year: 2024, month: 2 }.span(TODAY);
        assert_eq!(feb.1 - feb.0, 28);
        let oct = |d| Date { year: 2026, month: 10, day: d };
        assert_eq!(When::Days(oct(9), oct(3)).label(TODAY), "oct 3 – oct 9");
        assert_eq!(When::Days(oct(3), oct(3)).label(TODAY), "oct 3");
        assert_eq!(When::InMonth { year: 2026, month: 10 }.label(TODAY), "october");
        assert_eq!(When::InMonth { year: 2025, month: 12 }.label(TODAY), "dec 2025");
        assert_eq!(when_named("dec", TODAY), Some(When::InMonth { year: 2025, month: 12 }), "the last december that began");
        assert_eq!(when_named("may", TODAY), Some(When::InMonth { year: 2026, month: 5 }));
        assert_eq!(when_named("sept", TODAY), Some(When::InMonth { year: 2026, month: 9 }));
        assert_eq!(when_named("octo", TODAY), None);
        assert_eq!(when_named("this week", TODAY), Some(When::Week));

        assert_eq!(length_named("> 1 min"), Some(Length { min_ms: 60_000, max_ms: None }));
        assert_eq!(length_named("<30s"), Some(Length::SHORT));
        assert_eq!(length_named(">= 1,5 minutes"), Some(Length { min_ms: 90_000, max_ms: None }));
        assert_eq!(length_named("1-2 min"), Some(Length { min_ms: 60_000, max_ms: Some(120_000) }));
        assert_eq!(length_named("30 s – 2 min"), Some(Length::MEDIUM));
        assert_eq!(length_named("> 90"), None, "a unit is needed");
        assert_eq!(length_named("2-1 min"), None);
        assert_eq!(length_named("> 0 s"), None);
        assert_eq!(length_named("long"), None);
        let label = |l: Length| l.label();
        assert_eq!((label(Length::SHORT), label(Length::MEDIUM), label(Length::LONG)), ("< 30 s".into(), "30 s – 2 min".into(), "> 2 min".into()));
        assert_eq!(label(Length { min_ms: 90_000, max_ms: None }), "> 1.5 min");
        assert_eq!(label(Length { min_ms: 45_000, max_ms: None }), "> 45 s");
        assert_eq!(label(Length { min_ms: 0, max_ms: Some(3_600_000) }), "< 1 h");
    }

    #[test]
    fn the_search_box_names_filters_with_whole_phrases() {
        let items = items();
        let index = index(&items);
        let vocab = Vocabulary { index: &index, today: TODAY };
        let (tokens, words) = read("Tele this week > 1 min", &vocab);
        assert_eq!(tokens, [Token::When(When::Week), Token::Length(Length { min_ms: 60_000, max_ms: None })]);
        assert_eq!(words, ["tele"]);
        let (tokens, words) = read("afterglow gifs oct party", &vocab);
        assert_eq!(tokens, [Token::Maker(Maker::Afterglow), Token::Kind(Filter::Gifs), Token::When(When::InMonth { year: 2026, month: 10 })]);
        assert_eq!(words, ["party"]);
        let filters = Filters::default().with(&tokens);
        assert_eq!((filters.maker, filters.kind), (Some(Maker::Afterglow), Filter::Gifs));
        assert_eq!(filters.chips().len(), 2, "the kind has its own switch");

        // with one app's captures only, its name is just a word
        let glint_only: Vec<Item> = items.iter().filter(|i| i.maker == Maker::Glint).cloned().collect();
        let index = Index::new(&glint_only, |_| None);
        let (tokens, words) = read("glint today", &Vocabulary { index: &index, today: TODAY });
        assert_eq!((tokens, words), (vec![Token::When(When::Today)], vec!["glint".to_string()]));
    }

    #[test]
    fn suggestions_offer_what_the_last_words_could_become() {
        let items = items();
        let index = index(&items);
        let vocab = Vocabulary { index: &index, today: TODAY };
        let none = Filters::default();
        let tokens = |q: &str, f: &Filters| suggest(q, &vocab, f).into_iter().map(|s| (s.token, s.from)).collect::<Vec<_>>();
        assert_eq!(tokens("this w", &none), [(Token::When(When::Week), 0)]);
        assert_eq!(tokens("party yes", &none), [(Token::When(When::Yesterday), 6)]);
        assert_eq!(tokens("tel", &none), [(Token::App("Telegram Desktop".into()), 0)]);
        assert_eq!(tokens("strike", &none), [(Token::App("Counter-Strike 2".into()), 0)]);
        assert_eq!(
            tokens("> 2", &none),
            [(Token::Length(Length { min_ms: 2_000, max_ms: None }), 0), (Token::Length(Length { min_ms: 120_000, max_ms: None }), 0)]
        );
        assert_eq!(tokens("<1 mi", &none), [(Token::Length(Length { min_ms: 0, max_ms: Some(60_000) }), 0)]);
        assert_eq!(tokens("after", &none), [(Token::Maker(Maker::Afterglow), 0)]);
        assert_eq!(tokens("rec", &none), [(Token::Kind(Filter::Recordings), 0)]);
        assert_eq!(tokens("octo", &none), [(Token::When(When::InMonth { year: 2026, month: 10 }), 0)]);
        assert_eq!(tokens("lon", &none), [(Token::Length(Length::LONG), 0)]);
        // a whole phrase comes first
        assert_eq!(tokens("today", &none)[0], (Token::When(When::Today), 0));
        // nothing for what's set already, or for nothing typed
        let set = Filters { app: Some("Telegram Desktop".into()), ..Default::default() };
        assert!(tokens("tel", &set).is_empty());
        assert!(tokens("   ", &none).is_empty());
        assert!(tokens("zzz", &none).is_empty());
        assert!(suggest("d", &vocab, &none).len() <= MOST);
    }
}
