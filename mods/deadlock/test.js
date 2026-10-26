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

function hud() {
    const root = new Panel("CitadelHud", { classes: ["WindowRoot"] });
    const core = root.add(new Panel("Panel", { classes: ["HudCore"] }));
    const top = core.add(new Panel("CitadelHudTopBar", { id: "TopBar" }));
    const clock = top.add(label(["GameTime"], "3:07", "GameTime"));
    const team = top.add(new Panel("CitadelHudTopBarTeam", { id: "TeamFriendly" }));
    team.add(new Panel("CitadelHudTopBarPlayer", { classes: ["LocalPlayer"] })).classes.delete("LocalPlayer");
    const local = team.add(new Panel("CitadelHudTopBarPlayer", { classes: ["LocalPlayer"] }));
    local.add(label(["PlayerName"], "alexx"));
    local.add(label(["HeroName"], "Infernus"));
    const kills = local.add(label(["PlayerStat", "kills"], "0"));
    const deaths = local.add(label(["PlayerStat", "deaths"], "0"));
    const assists = local.add(label(["PlayerStat", "assists"], "0"));
    const feedRoot = core.add(new Panel("HudDataFeed", { id: "DataFeed", classes: ["DataFeed"] }));
    const feed = feedRoot.add(new Panel("Panel", { id: "EventFeed" }));
    const ann = core.add(new Panel("CitadelHudGameAnnouncements", { id: "CitadelHudGameAnnouncements" }));
    const toasts = ann.add(new Panel("ToastManager", { id: "ToastManager" }));
    return { root, clock, local, kills, deaths, assists, context: feedRoot, feed, toasts };
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

function teamMessage(text, good) {
    const entry = new Panel("CitadelHudTeamMsg");
    const msg = entry.add(new Panel("Panel", { id: "TeamMsg", classes: good ? ["IsPositive"] : [] }));
    const content = msg.add(new Panel("Panel", { classes: ["ObjectiveContent"] }));
    content.add(label(["messageMain"], text));
    content.add(label(["subText"], ""));
    return entry;
}

function run() {
    const h = hud();
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
    const RealDate = Date;
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
        return JSON.parse(l.slice("[AFTERGLOW]".length));
    });
    assert.strictEqual(RealDate, Date);
    return { h, tick, take, now: () => now };
}

const t = run();
const { h, tick, take } = t;

let out = take();
assert.deepStrictEqual(out.map((e) => e.event), ["hello", "phase"], "hello, then the phase; the feed already shown is skipped");
assert.strictEqual(out[0].version, "1.0.0");
assert.strictEqual(out[1].phase, "game");
assert.strictEqual(out[1].hero, "Infernus");
assert.strictEqual(out[1].v, 1);

tick(30);
assert.deepStrictEqual(take(), [], "nothing changes, nothing is written");

const killAt = t.now() + 100;
h.kills.text = "1";
tick();
h.feed.add(killEntry("alexx", "Bob", ["enemyDied"]));
tick(2);
out = take();
assert.deepStrictEqual(out.map((e) => e.event), ["feed", "kill"]);
assert.deepStrictEqual(out[0], { v: 1, event: "feed", killer: "alexx", victim: "Bob", assists: 0, me: "killer", tags: ["enemyDied"], t: out[0].t, clock: "3:07" });
assert.strictEqual(out[1].victim, "Bob");
assert.strictEqual(out[1].kills, 1);
assert.strictEqual(out[1].t, killAt, "the kill keeps the time the counter moved");

const deathAt = t.now() + 100;
h.local.classes.add("Dead");
h.deaths.text = "1";
tick();
h.feed.add(killEntry("Bob", "alexx", ["teammateDied"], 2));
tick(3);
out = take();
assert.deepStrictEqual(out.map((e) => e.event), ["feed", "death"], "one death for the class and the counter");
assert.strictEqual(out[0].me, "victim");
assert.strictEqual(out[0].assists, 2);
assert.strictEqual(out[1].killer, "Bob");
assert.strictEqual(out[1].deaths, 1);
assert.strictEqual(out[1].t, deathAt);

h.local.classes.delete("Dead");
tick();
assert.deepStrictEqual(take().map((e) => e.event), ["respawn"]);

h.assists.text = "1";
h.feed.add(killEntry("Carl", "Dan", ["enemyDied", "hasAssists"], 1));
tick(2);
out = take();
assert.deepStrictEqual(out.map((e) => e.event), ["feed", "assist"]);
assert.strictEqual(out[1].victim, "Dan");

h.kills.text = "2";
tick(12);
out = take();
assert.deepStrictEqual(out.map((e) => e.event), ["kill"], "a kill the feed never showed still comes, after a second");
assert.strictEqual(out[0].victim, undefined);

h.feed.add(teamMessage("Your team destroyed the <b>Guardian</b>", true));
h.feed.add(killEntry("alexx", "Guardian", ["bossDied"]));
tick();
out = take();
assert.deepStrictEqual(out.map((e) => [e.event, e.text || e.what, e.good, e.mine]), [
    ["objective", "Your team destroyed the Guardian", true, undefined],
    ["objective", "bossDied", undefined, true],
]);

const toast = h.toasts.add(new Panel("Panel", { classes: ["GenericAnnouncement"] }));
toast.add(label(["AnnouncementTitle"], "Soul Urn"));
toast.add(label(["AnnouncementDescription"], "The urn has spawned"));
tick(3);
out = take();
assert.deepStrictEqual(out.map((e) => [e.event, e.src, e.text, e.sub]), [["objective", "announce", "Soul Urn", "The urn has spawned"]]);

const late = killEntry("", "", ["enemyDied"]);
h.feed.add(late);
tick();
late.FindChildrenWithClassTraverse("killerInfo")[0].text = "Eve";
late.FindChildrenWithClassTraverse("victimInfo")[0].text = "Bob";
tick();
out = take();
assert.deepStrictEqual(out.map((e) => [e.event, e.killer, e.me]), [["feed", "Eve", undefined]], "an entry is read once its names are in");

h.root.classes.add("GameStatePostGame");
tick();
assert.deepStrictEqual(take().map((e) => [e.event, e.phase]), [["phase", "postgame"]]);

for (const p of h.feed.Children()) p.remove();
tick(5);
assert.deepStrictEqual(take(), [], "entries fading out say nothing");

h.context.gone = true;
tick();
assert.deepStrictEqual(take(), []);

const u = run();
u.take();
u.h.kills.text = "-";
u.h.deaths.text = "-";
u.tick();
u.h.feed.add(killEntry("alexx", "Bob", ["enemyDied"]));
u.tick();
assert.deepStrictEqual(u.take().map((e) => [e.event, e.src, e.victim]), [["feed", undefined, "Bob"], ["kill", "feed", "Bob"]], "unreadable counters: kills come from the feed");

console.log("events.js: ok");
