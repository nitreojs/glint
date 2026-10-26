"use strict";

const assert = require("assert");
const fs = require("fs");
const path = require("path");

class Panel {
    constructor(paneltype, opts = {}) {
        this.paneltype = paneltype;
        this.id = opts.id || "";
        this.classes = new Set(opts.classes || []);
        this.text = opts.text;
        if (opts.heroid !== undefined) this.heroid = opts.heroid;
        this.kids = [];
        this.parent = null;
        this.gone = false;
    }
    add(child) {
        child.parent = this;
        this.kids.push(child);
        return child;
    }
    remove() {
        this.gone = true;
        this.parent.kids = this.parent.kids.filter((k) => k !== this);
    }
    IsValid() {
        return !this.gone;
    }
    BHasClass(c) {
        return this.classes.has(c);
    }
    GetParent() {
        return this.parent;
    }
    Children() {
        return this.kids.slice();
    }
    FindChildTraverse(id) {
        for (const k of this.kids) {
            if (k.id === id) return k;
            const f = k.FindChildTraverse(id);
            if (f) return f;
        }
        return null;
    }
    FindChildrenWithClassTraverse(c) {
        const out = [];
        for (const k of this.kids) {
            if (k.classes.has(c)) out.push(k);
            out.push(...k.FindChildrenWithClassTraverse(c));
        }
        return out;
    }
}

const label = (classes, text, id) => new Panel("Label", { classes, text, id });

function player(team, name, hero, local) {
    const p = team.add(new Panel("CitadelHudTopBarPlayer", { classes: local ? ["LocalPlayer"] : [] }));
    p.add(new Panel("CitadelHeroBadge", { id: "HeroBadge", heroid: local ? 7 : 3 }));
    const streak = p.add(new Panel("Panel", { id: "KillStreakContainer" }));
    const num = streak.add(label(["KillStreakNum"], "0"));
    const labels = {
        name: p.add(label(["PlayerName"], name)),
        hero: p.add(label(["HeroName"], hero)),
        level: p.add(label(["HeroLevelLabel"], "17")),
        souls: p.add(label(["SoulsValue"], "21.4k", "SoulsValue")),
        kills: p.add(label(["PlayerStat", "kills"], "0")),
        deaths: p.add(label(["PlayerStat", "deaths"], "0")),
        assists: p.add(label(["PlayerStat", "assists"], "0")),
    };
    p.add(label(["objDamageLabel", "objDamageTitle"], "obj dmg"));
    p.add(label(["objDamageLabel"], "5321"));
    return { panel: p, num, ...labels };
}

function objective(container, classes, name) {
    const o = container.add(new Panel("Panel", { classes: ["objective", ...classes] }));
    o.add(label([], name, "name"));
    return o;
}

function hud() {
    const root = new Panel("CitadelHud", { classes: ["WindowRoot"] });
    const core = root.add(new Panel("Panel", { classes: ["HudCore"] }));
    const top = core.add(new Panel("CitadelHudTopBar", { id: "TopBar" }));
    const clock = top.add(label(["GameTime"], "3:07", "GameTime"));
    const teams = top.add(new Panel("Panel", { id: "TeamsContainer" }));
    const friendly = teams.add(new Panel("CitadelHudTopBarTeam", { id: "TeamFriendly" })).add(new Panel("Panel", { id: "PlayersContainer" }));
    const enemy = teams.add(new Panel("CitadelHudTopBarTeam", { id: "TeamEnemy" })).add(new Panel("Panel", { id: "PlayersContainer" }));
    const mate = player(friendly, "Carl", "Abrams", false);
    const me = player(friendly, "alexx", "Infernus", true);
    const bob = player(enemy, "Bob", "Haze", false);
    const map = top.add(new Panel("CitadelObjectivesMap", { id: "ObjectivesMap", classes: ["Team1IsFriend", "Team2IsEnemy", "LiveGame"] }));
    const icons = {};
    for (const team of [1, 2]) {
        for (const id of ["Core", "Titan", "Tier2_1", "Tier2_2", "Tier1_1", "Tier1_2"]) {
            icons[`Team${team}${id}`] = map.add(new Panel("Panel", { id: `Team${team}${id}`, classes: ["Icon", "Alive"] }));
        }
    }
    const gameplay = core.add(new Panel("Panel", { id: "gameplay_hud" }));
    const health = gameplay.add(new Panel("TeamObjectiveHealthPanel", { id: "objectives_health_friendly" }));
    const ours = health.add(new Panel("Panel", { id: "team_friendly_container", classes: ["team_container"] }));
    const neutral = health.add(new Panel("Panel", { id: "team_neutral_container", classes: ["team_container", "neutral"] }));
    const guardian = objective(ours, ["is_tier1", "yellowLane", "is_active"], "Guardian");
    const midboss = objective(neutral, ["is_mid", "is_neutral", "is_active"], "Mid-Boss");
    const level = gameplay.add(new Panel("CitadelPlayerLevel"));
    level.add(new Panel("Panel", { classes: ["PlayerLevelContainer"] }));
    level.add(new Panel("Panel", { id: "LevelAmount" })).add(new Panel("CitadelHeroImage", { id: "HeroImage", heroid: 12 }));
    const matchEnd = core.add(new Panel("CitadelHudMatchEnd", { id: "MatchEnd" }));
    const results = matchEnd.add(new Panel("Panel", { classes: ["MatchComplete"] }));
    results.add(label([], "", "TeamResultsTop"));
    results.add(label([], "", "TeamResultsBottom"));
    const feedRoot = core.add(new Panel("HudDataFeed", { id: "DataFeed", classes: ["DataFeed"] }));
    const feed = feedRoot.add(new Panel("Panel", { id: "EventFeed" }));
    const ann = core.add(new Panel("CitadelHudGameAnnouncements", { id: "CitadelHudGameAnnouncements" }));
    const toasts = ann.add(new Panel("ToastManager", { id: "ToastManager" }));
    return { root, clock, me, mate, bob, icons, guardian, midboss, matchEnd, local: me.panel, kills: me.kills, deaths: me.deaths, assists: me.assists, context: feedRoot, feed, toasts };
}

function killEntry(killer, victim, tags, assisters = 0) {
    const entry = new Panel("Panel", { classes: ["data_entry"] });
    const info = entry.add(new Panel("CitadelHudInfoFeed", { classes: tags }));
    const k = info.add(new Panel("Panel", { id: "KillerContainer", classes: ["killerContainer"] }));
    k.add(label(["killerInfo", "personaName"], killer));
    const a = k.add(new Panel("Panel", { id: "AssistsContainer" }));
    for (let i = 0; i < assisters; i++) a.add(new Panel("CitadelHeroImage"));
    info.add(new Panel("Panel", { id: "VictimContainer", classes: ["victimContainer"] })).add(label(["victimInfo", "personaName"], victim));
    return entry;
}

function teamMessage(text, sub, good) {
    const entry = new Panel("CitadelHudTeamMsg");
    const msg = entry.add(new Panel("Panel", { id: "TeamMsg", classes: good ? ["IsPositive"] : [] }));
    const content = msg.add(new Panel("Panel", { classes: ["ObjectiveContent"] }));
    content.add(label(["messageMain"], text));
    content.add(label(["subText"], sub));
    return entry;
}

function run(setup = () => {}) {
    const h = hud();
    setup(h);
    const lines = [];
    let scheduled = null;
    let now = 1_700_000_000_000;
    const $ = {
        GetContextPanel: () => h.context,
        Msg: (s) => lines.push(s),
        Schedule: (_s, fn) => {
            scheduled = fn;
        },
    };
    const FakeDate = { now: () => now };
    const src = fs.readFileSync(path.join(__dirname, "src", "events.js"), "utf8");
    h.feed.add(killEntry("old", "stale", ["enemyDied"]));
    new Function("$", "Date", src)($, FakeDate);
    const tick = (n = 1) => {
        for (let i = 0; i < n; i++) {
            now += 100;
            const fn = scheduled;
            scheduled = null;
            fn();
        }
    };
    const take = () => lines.splice(0).map((l) => {
        assert.ok(l.startsWith("[AFTERGLOW]{"), l);
        const e = JSON.parse(l.slice("[AFTERGLOW]".length));
        assert.strictEqual(e.v, 2, l);
        return e;
    });
    return { h, tick, take, now: () => now };
}

const pick = (e, keys) => Object.fromEntries(keys.filter((k) => e[k] !== undefined).map((k) => [k, e[k]]));

const t = run();
const { h, tick, take } = t;

let out = take();
assert.deepStrictEqual(out.map((e) => e.event), ["hello", "phase"], "hello, then the phase; what was already shown is skipped");
assert.strictEqual(out[0].version, "2.0.0");
assert.deepStrictEqual(pick(out[1], ["phase", "hero", "hero_id"]), { phase: "game", hero: "Infernus", hero_id: 7 });

tick(30);
assert.deepStrictEqual(take(), [], "nothing changes, nothing is written");

const killAt = t.now() + 100;
h.kills.text = "1";
tick();
h.feed.add(killEntry("ALEXX", "BOB", ["enemyDied"]));
tick(2);
out = take();
assert.deepStrictEqual(out.map((e) => e.event), ["feed", "kill"]);
assert.deepStrictEqual(pick(out[0], ["killer", "victim", "assists", "me", "tags", "clock"]), { killer: "ALEXX", victim: "BOB", assists: 0, me: "killer", tags: ["enemyDied"], clock: "3:07" }, "the feed's capitals are the player's name");
assert.deepStrictEqual(pick(out[1], ["victim", "kills", "t"]), { victim: "BOB", kills: 1, t: killAt });

const deathAt = t.now() + 100;
h.local.classes.add("Dead");
h.deaths.text = "1";
tick();
h.feed.add(killEntry("Bob", "alexx", ["teammateDied"], 2));
tick(3);
out = take();
assert.deepStrictEqual(out.map((e) => e.event), ["feed", "death"], "one death for the class and the counter");
assert.deepStrictEqual(pick(out[1], ["killer", "deaths", "t"]), { killer: "Bob", deaths: 1, t: deathAt });

h.local.classes.delete("Dead");
tick();
assert.deepStrictEqual(take().map((e) => e.event), ["respawn"]);

h.assists.text = "1";
h.feed.add(killEntry("Carl", "Dan", ["enemyDied", "hasAssists"], 1));
tick(2);
out = take();
assert.deepStrictEqual(out.map((e) => [e.event, e.victim]), [["feed", "Dan"], ["assist", "Dan"]]);

h.kills.text = "2";
tick(12);
out = take();
assert.deepStrictEqual(out.map((e) => [e.event, e.victim]), [["kill", undefined]], "a kill the feed never showed still comes, after a second");

h.local.classes.add("IsFirstBlood");
tick();
h.bob.panel.classes.add("KillStreak");
h.bob.num.text = "3";
tick();
h.bob.num.text = "4";
tick();
h.bob.panel.classes.delete("KillStreak");
h.bob.num.text = "0";
h.local.classes.add("KillStreak");
h.me.num.text = "3";
tick();
out = take();
assert.deepStrictEqual(out.map((e) => pick(e, ["event", "kind", "count", "name", "hero", "mine"])), [
    { event: "streak", kind: "first_blood", name: "alexx", hero: "Infernus", mine: true },
    { event: "streak", kind: "streak", count: 3, name: "Bob", hero: "Haze", mine: false },
    { event: "streak", kind: "streak", count: 4, name: "Bob", hero: "Haze", mine: false },
    { event: "streak", kind: "streak", count: 3, name: "alexx", hero: "Infernus", mine: true },
    { event: "streak", kind: "ended", count: 4, name: "Bob", hero: "Haze", mine: false },
]);
tick(25);
assert.deepStrictEqual(take(), [], "rescanning the top bar says nothing new");

h.icons.Team2Tier1_2.classes.delete("Alive");
tick();
out = take();
assert.deepStrictEqual(out.map((e) => pick(e, ["event", "state", "what", "side", "team", "lane", "id"])), [
    { event: "objective", state: "destroyed", what: "guardian", side: "theirs", team: 2, lane: 2, id: "Team2Tier1_2" },
]);
h.icons.Team1Titan.classes.delete("Alive");
tick();
assert.deepStrictEqual(take().map((e) => [e.what, e.side]), [["patron", "ours"]]);
for (const id of ["Team1Core", "Team1Tier2_1", "Team1Tier2_2"]) h.icons[id].classes.delete("Alive");
tick();
assert.deepStrictEqual(take(), [], "the map clearing at once is no fight");

h.guardian.classes.add("on_damaged");
tick();
h.guardian.classes.delete("on_damaged");
tick();
h.guardian.classes.add("on_damaged");
tick();
out = take();
assert.deepStrictEqual(out.map((e) => pick(e, ["event", "state", "what", "side", "lane", "name"])), [
    { event: "objective", state: "attacked", what: "guardian", side: "ours", lane: "yellow", name: "Guardian" },
], "attacked at most once per objective in 20 s");
h.guardian.classes.delete("on_damaged");
tick(200);
h.guardian.classes.add("on_damaged");
tick();
assert.deepStrictEqual(take().map((e) => e.state), ["attacked"]);
h.guardian.classes.add("is_dead");
tick();
assert.deepStrictEqual(take(), [], "a guardian's fall comes from the map");

h.midboss.classes.add("is_dead");
tick();
h.feed.add(killEntry("alexx", "Mid-Boss", ["midBoss", "friendly"]));
tick();
out = take();
assert.deepStrictEqual(out.map((e) => pick(e, ["event", "state", "what", "side"])), [{ event: "objective", state: "destroyed", what: "midboss", side: "neutral" }], "told once, by whichever comes first");

h.feed.add(killEntry("alexx", "Bob", ["IdolReturned", "friendly"]));
h.feed.add(teamMessage("Enemy Yellow Tier 1", "DESTROYED", true));
tick();
const toast = h.toasts.add(new Panel("Panel", { classes: ["GenericAnnouncement"] }));
toast.add(label(["AnnouncementTitle"], "Soul Urn"));
toast.add(label(["AnnouncementDescription"], "The urn has spawned"));
tick(2);
out = take();
assert.deepStrictEqual(out.map((e) => pick(e, ["event", "state", "what", "side", "mine", "text", "sub", "good", "src"])), [
    { event: "objective", state: "returned", what: "idol", side: "ours", mine: true },
    { event: "announce", text: "Enemy Yellow Tier 1", sub: "DESTROYED", good: true, src: "team" },
    { event: "announce", text: "Soul Urn", sub: "The urn has spawned", src: "toast" },
]);

const late = killEntry("", "", ["enemyDied"]);
h.feed.add(late);
tick();
late.FindChildrenWithClassTraverse("killerInfo")[0].text = "Eve";
late.FindChildrenWithClassTraverse("victimInfo")[0].text = "Bob";
tick();
assert.deepStrictEqual(take().map((e) => [e.event, e.killer, e.me]), [["feed", "Eve", undefined]], "an entry is read once its names are in");

h.matchEnd.classes.add("LocalPlayerTeam1");
h.matchEnd.classes.add("Team1Victory");
h.matchEnd.FindChildTraverse("TeamResultsTop").text = '<span class="TeamName">The Hidden King</span>';
h.matchEnd.FindChildTraverse("TeamResultsBottom").text = "Victory";
h.matchEnd.classes.add("ShowMatchEnd");
tick();
out = take();
assert.deepStrictEqual(out.map((e) => pick(e, ["event", "result", "team", "winner", "text"])), [
    { event: "result", result: "victory", team: 1, winner: 1, text: "The Hidden King Victory" },
    { event: "summary" },
]);
assert.deepStrictEqual(pick(out[1], ["name", "hero", "hero_id", "level", "souls", "objective_damage", "kills", "deaths", "assists", "best_streak", "src"]), {
    name: "alexx", hero: "Infernus", hero_id: 7, level: 17, souls: "21.4k", objective_damage: 5321, kills: 2, deaths: 1, assists: 1, best_streak: 3, src: "kda",
});
h.root.classes.add("GameStatePostGame");
tick(150);
assert.deepStrictEqual(take().map((e) => [e.event, e.phase]), [["phase", "postgame"]], "one summary a match");

for (const p of h.feed.Children()) p.remove();
tick(5);
assert.deepStrictEqual(take(), [], "entries fading out say nothing");
h.context.gone = true;
tick();
assert.deepStrictEqual(take(), []);

const lost = run((x) => {
    x.matchEnd.classes.add("LocalPlayerTeam2");
});
lost.take();
lost.h.matchEnd.classes.add("Team1Victory");
lost.h.matchEnd.classes.add("ShowMatchEnd");
lost.tick();
assert.deepStrictEqual(lost.take().map((e) => [e.event, e.result]), [["result", "defeat"], ["summary", undefined]]);

const ended = run();
ended.take();
ended.h.root.classes.add("GameStatePostGame");
ended.tick(50);
assert.deepStrictEqual(ended.take().map((e) => e.event), ["phase"]);
ended.tick(60);
assert.deepStrictEqual(ended.take().map((e) => e.event), ["summary"], "no result shown: the summary comes 10 s into the post-game");

const hideout = run((x) => {
    x.me.name.text = "#";
    x.me.hero.text = "{s:hero_name}";
    x.local.FindChildTraverse("HeroBadge").heroid = 0;
});
let lines = hideout.take();
assert.deepStrictEqual(pick(lines[1], ["phase", "hero", "hero_id"]), { phase: "game", hero_id: 12 }, "no names in the top bar: the hero comes from the level panel");
hideout.h.feed.add(killEntry("MEMORIES", "ABRAMS", ["enemyDied"]));
hideout.tick();
lines = hideout.take();
assert.deepStrictEqual(lines.map((e) => [e.event, e.killer, e.me]), [["feed", "MEMORIES", undefined]], "left to afterglow, which knows the steam name");

const u = run();
u.take();
u.h.kills.text = "-";
u.h.deaths.text = "-";
u.tick();
u.h.feed.add(killEntry("alexx", "Bob", ["enemyDied"]));
u.tick();
assert.deepStrictEqual(u.take().map((e) => [e.event, e.src, e.victim]), [["feed", undefined, "Bob"], ["kill", "feed", "Bob"]], "unreadable counters: kills come from the feed");

console.log("events.js: ok");
