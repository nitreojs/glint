(function () {
    "use strict";

    var VERSION = "2.0.0";
    var SCHEMA = 2;
    var TAG = "[AFTERGLOW]";
    var POLL = 0.1;
    var WAIT_POLLS = 10;
    var DEATH_GAP_MS = 5000;
    var ATTACK_GAP_MS = 20000;
    var SAME_OBJECTIVE_MS = 10000;
    var RESCAN_MS = 2000;
    var SUMMARY_WAIT_MS = 10000;
    var FEED_TAGS = [
        "enemyDied", "teammateDied", "killedSelf", "noKiller", "hasAssists", "has_killstreak", "friendly", "enemy",
        "bossDied", "midBoss", "IdolPickedUp", "IdolReturned", "KothCompleted", "SoulBagPickup", "SoulBagStolen",
        "isRejuvenatorPickup", "isRejuvenatorUsed", "is_item"
    ];
    var FEED_OBJECTIVES = {
        midBoss: ["midboss", "destroyed"],
        bossDied: ["boss", "destroyed"],
        IdolPickedUp: ["idol", "picked_up"],
        IdolReturned: ["idol", "returned"],
        KothCompleted: ["koth", "completed"],
        isRejuvenatorPickup: ["rejuvenator", "picked_up"],
        isRejuvenatorUsed: ["rejuvenator", "used"]
    };
    var MAP_KINDS = { Tier1: "guardian", Tier2: "walker", Titan: "patron", Core: "core" };
    var HEALTH_KINDS = [
        ["is_titan", "patron"], ["is_shield_generator", "shrine"], ["is_barracks_boss", "base_guardian"],
        ["is_tier2", "walker"], ["is_tier1", "guardian"], ["is_mid", "midboss"], ["is_generic_boss", "boss"]
    ];
    var LANES = [["yellowLane", "yellow"], ["greenLane", "green"], ["blueLane", "blue"], ["purpleLane", "purple"]];
    var PHASES = [["GameStatePostGame", "postgame"], ["GameStatePreGameWait", "pregame_wait"], ["GameStatePreGame", "pregame"]];

    var context = $.GetContextPanel();
    var root = null;
    var topBar = null;
    var feed = null;
    var announcements = null;
    var healthRoot = null;
    var matchEnd = null;
    var clock = null;
    var me = null;
    var heroId = null;
    var heroLookedAt = 0;
    var phase = null;
    var feedSeen = [];
    var feedWaiting = [];
    var recent = [];
    var pending = [];
    var toastsSeen = [];
    var players = [];
    var playersAt = 0;
    var map = null;
    var healths = {};
    var objectivesTold = {};
    var ended = false;
    var summarized = false;
    var postgameAt = 0;
    var bestStreak = 0;
    var tally = { kills: 0, deaths: 0, assists: 0 };
    var errors = {};
    var lastDeath = 0;

    function emit(event, fields) {
        var out = { v: SCHEMA, event: event };
        for (var k in fields) {
            if (fields[k] !== null && fields[k] !== undefined) {
                out[k] = fields[k];
            }
        }
        if (out.t === undefined) {
            out.t = Date.now();
        }
        var c = text(clock);
        if (c && out.clock === undefined) {
            out.clock = c;
        }
        $.Msg(TAG + JSON.stringify(out));
    }

    function valid(p) {
        try {
            return !!(p && p.IsValid && p.IsValid());
        } catch (e) {
            return false;
        }
    }

    function has(p, c) {
        try {
            return valid(p) && p.BHasClass(c);
        } catch (e) {
            return false;
        }
    }

    function hasUp(p, c, depth) {
        for (var i = 0; i <= depth && valid(p); i++) {
            if (has(p, c)) {
                return true;
            }
            p = parent(p);
        }
        return false;
    }

    function parent(p) {
        try {
            return p.GetParent();
        } catch (e) {
            return null;
        }
    }

    function text(p) {
        try {
            if (!valid(p) || typeof p.text !== "string") {
                return null;
            }
            var t = p.text.replace(/<[^>]*>/g, " ").replace(/\s+/g, " ").trim();
            return t === "" || t.charAt(0) === "#" || t.indexOf("{") >= 0 ? null : t;
        } catch (e) {
            return null;
        }
    }

    function same(a, b) {
        return a !== null && b !== null && a !== undefined && b !== undefined && a.toLowerCase() === b.toLowerCase();
    }

    function byClass(p, c) {
        try {
            return valid(p) ? p.FindChildrenWithClassTraverse(c) : [];
        } catch (e) {
            return [];
        }
    }

    function byId(p, id) {
        try {
            return valid(p) ? p.FindChildTraverse(id) : null;
        } catch (e) {
            return null;
        }
    }

    function firstOf(p, c) {
        var found = byClass(p, c);
        return found.length > 0 ? found[0] : null;
    }

    function classText(p, c) {
        return text(firstOf(p, c));
    }

    function children(p) {
        try {
            return valid(p) ? p.Children() : [];
        } catch (e) {
            return [];
        }
    }

    function count(s) {
        var m = s === null ? null : /^(\d+)$/.exec(s);
        return m ? parseInt(m[1], 10) : null;
    }

    function contains(list, p) {
        for (var i = 0; i < list.length; i++) {
            if (list[i] === p) {
                return true;
            }
        }
        return false;
    }

    function alive(list) {
        var out = [];
        for (var i = 0; i < list.length; i++) {
            if (valid(list[i])) {
                out.push(list[i]);
            }
        }
        return out;
    }

    function ancestors() {
        var out = [];
        var p = context;
        while (valid(p) && out.length < 64) {
            out.push(p);
            p = parent(p);
        }
        return out;
    }

    function ofType(p, type, depth) {
        if (!valid(p)) {
            return null;
        }
        if (p.paneltype === type) {
            return p;
        }
        if (depth <= 0) {
            return null;
        }
        var kids = children(p);
        for (var i = 0; i < kids.length; i++) {
            var found = ofType(kids[i], type, depth - 1);
            if (found) {
                return found;
            }
        }
        return null;
    }

    function allOfType(p, type, depth, out) {
        if (!valid(p)) {
            return out;
        }
        if (p.paneltype === type) {
            out.push(p);
            return out;
        }
        if (depth > 0) {
            var kids = children(p);
            for (var i = 0; i < kids.length; i++) {
                allOfType(kids[i], type, depth - 1, out);
            }
        }
        return out;
    }

    function heroIdOf(p) {
        if (!valid(p)) {
            return null;
        }
        var tries = [
            function () { return p.heroid; },
            function () { return p.heroID; },
            function () { return p.GetAttributeInt("heroid", 0); }
        ];
        for (var i = 0; i < tries.length; i++) {
            try {
                var v = Number(tries[i]());
                if (isFinite(v) && v > 0 && Math.floor(v) === v) {
                    return v;
                }
            } catch (e) {
            }
        }
        return null;
    }

    function lookForHero() {
        var now = Date.now();
        if (heroId !== null || now - heroLookedAt < 5000) {
            return;
        }
        heroLookedAt = now;
        heroId = heroIdOf(me ? byId(me.panel, "HeroBadge") : null);
        if (heroId === null) {
            var level = firstOf(root, "PlayerLevelContainer");
            heroId = heroIdOf(byId(parent(level), "HeroImage"));
        }
    }

    function findLocal() {
        var found = byClass(topBar, "LocalPlayer");
        for (var i = 0; i < found.length; i++) {
            if (found[i].paneltype === "CitadelHudTopBarPlayer") {
                return found[i];
            }
        }
        return null;
    }

    function labelsOf(panel) {
        var damage = null;
        var labels = byClass(panel, "objDamageLabel");
        for (var i = 0; i < labels.length; i++) {
            if (!has(labels[i], "objDamageTitle")) {
                damage = labels[i];
            }
        }
        return {
            name: firstOf(panel, "PlayerName"),
            hero: firstOf(panel, "HeroName"),
            kills: firstOf(panel, "kills"),
            deaths: firstOf(panel, "deaths"),
            assists: firstOf(panel, "assists"),
            level: firstOf(panel, "HeroLevelLabel"),
            souls: byId(panel, "SoulsValue"),
            damage: damage
        };
    }

    function readMe(panel, labels) {
        return {
            panel: panel,
            labels: labels,
            name: text(labels.name),
            hero: text(labels.hero),
            kills: count(text(labels.kills)),
            deaths: count(text(labels.deaths)),
            assists: count(text(labels.assists)),
            dead: has(panel, "Dead")
        };
    }

    function currentPhase(chain) {
        for (var i = 0; i < PHASES.length; i++) {
            for (var j = 0; j < chain.length; j++) {
                if (has(chain[j], PHASES[i][0])) {
                    return PHASES[i][1];
                }
            }
        }
        return me ? "game" : null;
    }

    function queue(event, fields, want) {
        pending.push({ event: event, fields: fields, want: want, t: Date.now(), polls: 0 });
    }

    function lastEntry(fits) {
        var now = Date.now();
        for (var i = recent.length - 1; i >= 0; i--) {
            var r = recent[i];
            if (now - r.t <= 3000 && !r.used && fits(r)) {
                return r;
            }
        }
        return null;
    }

    function settle() {
        var keep = [];
        for (var i = 0; i < pending.length; i++) {
            var p = pending[i];
            var hit = null;
            if (p.want === "victim") {
                hit = lastEntry(function (r) { return r.me === "killer"; });
                if (hit) {
                    p.fields.victim = hit.entry.victim;
                }
            } else if (p.want === "killer") {
                hit = lastEntry(function (r) { return r.me === "victim"; });
                if (hit) {
                    p.fields.killer = hit.entry.killer;
                }
            } else if (p.want === "assisted") {
                hit = lastEntry(function (r) { return r.me === null && r.entry.tags.indexOf("enemyDied") >= 0; });
                if (hit) {
                    p.fields.victim = hit.entry.victim;
                }
            }
            if (hit || ++p.polls >= WAIT_POLLS) {
                if (hit) {
                    hit.used = true;
                }
                p.fields.t = p.t;
                emit(p.event, p.fields);
            } else {
                keep.push(p);
            }
        }
        pending = keep;
    }

    function died(deaths, src) {
        var now = Date.now();
        if (now - lastDeath > DEATH_GAP_MS) {
            tally.deaths++;
            queue("death", { deaths: deaths, src: src }, "killer");
        }
        lastDeath = now;
    }

    function trackMe() {
        var panel = me && valid(me.panel) ? me.panel : findLocal();
        if (!panel) {
            me = null;
            return;
        }
        var now = readMe(panel, me && me.panel === panel ? me.labels : labelsOf(panel));
        if (!me || me.panel !== panel) {
            me = now;
            return;
        }
        var counted = now.kills !== null && now.deaths !== null && now.assists !== null;
        if (counted && me.kills !== null && now.kills > me.kills) {
            for (var k = me.kills + 1; k <= now.kills; k++) {
                tally.kills++;
                queue("kill", { kills: k, src: "kda" }, "victim");
            }
        }
        if (counted && me.assists !== null && now.assists > me.assists) {
            for (var a = me.assists + 1; a <= now.assists; a++) {
                tally.assists++;
                queue("assist", { assists: a }, "assisted");
            }
        }
        if (now.dead && !me.dead) {
            died(now.deaths, "dead");
        } else if (!now.dead && me.dead) {
            emit("respawn", {});
        }
        if (counted && me.deaths !== null && now.deaths > me.deaths) {
            died(now.deaths, "kda");
        }
        me = now;
    }

    function trackStreaks(quiet) {
        var now = Date.now();
        var stale = false;
        for (var i = 0; i < players.length; i++) {
            stale = stale || !valid(players[i].panel);
        }
        if (stale || now - playersAt > RESCAN_MS) {
            playersAt = now;
            var panels = allOfType(byId(topBar, "TeamsContainer") || topBar, "CitadelHudTopBarPlayer", 8, []);
            var next = [];
            for (var j = 0; j < panels.length; j++) {
                var known = null;
                for (var k = 0; k < players.length; k++) {
                    if (players[k].panel === panels[j]) {
                        known = players[k];
                    }
                }
                next.push(known || streakState(panels[j], { panel: panels[j], name: firstOf(panels[j], "PlayerName"), hero: firstOf(panels[j], "HeroName"), num: firstOf(panels[j], "KillStreakNum") }));
            }
            players = next;
        }
        for (var n = 0; n < players.length; n++) {
            var s = players[n];
            var was = { streak: s.streak, first: s.first, count: s.count };
            streakState(s.panel, s);
            if (quiet) {
                continue;
            }
            var mine = has(s.panel, "LocalPlayer") || (me !== null && same(text(s.name), me.name));
            var who = { name: text(s.name), hero: text(s.hero), mine: mine };
            if (s.first && !was.first) {
                emit("streak", merge({ kind: "first_blood" }, who));
            }
            if (s.streak && s.count !== null && s.count > 0 && (!was.streak || was.count === null || s.count > was.count)) {
                if (mine) {
                    bestStreak = Math.max(bestStreak, s.count);
                }
                emit("streak", merge({ kind: "streak", count: s.count }, who));
            } else if (!s.streak && was.streak) {
                emit("streak", merge({ kind: "ended", count: was.count }, who));
            }
        }
    }

    function streakState(panel, s) {
        s.streak = has(panel, "KillStreak");
        s.first = has(panel, "IsFirstBlood");
        s.count = count(text(s.num));
        return s;
    }

    function merge(a, b) {
        for (var k in b) {
            a[k] = b[k];
        }
        return a;
    }

    function objectiveKey(what, side, lane) {
        return what === "midboss" ? what : [what, side, lane].join(":");
    }

    function toldRecently(key, now) {
        if (objectivesTold[key] !== undefined && now - objectivesTold[key] < SAME_OBJECTIVE_MS) {
            return true;
        }
        objectivesTold[key] = now;
        return false;
    }

    function trackMap(quiet) {
        if (!map || !valid(map.panel)) {
            var panel = byId(topBar, "ObjectivesMap");
            map = null;
            if (!panel) {
                return;
            }
            var icons = byClass(panel, "Icon");
            map = { panel: panel, icons: [] };
            for (var i = 0; i < icons.length; i++) {
                map.icons.push({ panel: icons[i], id: icons[i].id, alive: has(icons[i], "Alive") });
            }
            return;
        }
        var fallen = [];
        for (var j = 0; j < map.icons.length; j++) {
            var icon = map.icons[j];
            var up = has(icon.panel, "Alive");
            if (icon.alive && !up) {
                fallen.push(icon);
            }
            icon.alive = up;
        }
        if (quiet || phase === "postgame" || has(map.panel, "MatchComplete") || fallen.length > 2) {
            return;
        }
        var now = Date.now();
        for (var f = 0; f < fallen.length; f++) {
            var m = /^Team([12])(Core|Titan|Tier1|Tier2)(?:_(\d))?$/.exec(fallen[f].id || "");
            if (!m) {
                continue;
            }
            var team = parseInt(m[1], 10);
            var side = hasUp(map.panel, "Team" + team + "IsFriend", 3) ? "ours" : hasUp(map.panel, "Team" + team + "IsEnemy", 3) ? "theirs" : null;
            var what = MAP_KINDS[m[2]];
            if (toldRecently(what + ":" + side + ":" + team + ":" + (m[3] || ""), now)) {
                continue;
            }
            emit("objective", { state: "destroyed", what: what, side: side, team: team, lane: m[3] ? parseInt(m[3], 10) : null, id: fallen[f].id });
        }
    }

    function healthSide(p) {
        for (var i = 0; i < 8 && valid(p); i++) {
            if (has(p, "friend") || p.id === "team_friendly_container") {
                return "ours";
            }
            if (p.id === "team_enemy_container") {
                return "theirs";
            }
            if (has(p, "is_neutral") || has(p, "neutral") || p.id === "team_neutral_container") {
                return "neutral";
            }
            p = parent(p);
        }
        return null;
    }

    function trackHealth(quiet) {
        if (!valid(healthRoot)) {
            healthRoot = byId(root, "objectives_health_friendly");
        }
        var panels = byClass(healthRoot, "objective");
        var now = Date.now();
        for (var i = 0; i < panels.length; i++) {
            var p = panels[i];
            var what = "objective";
            for (var k = 0; k < HEALTH_KINDS.length; k++) {
                if (has(p, HEALTH_KINDS[k][0])) {
                    what = HEALTH_KINDS[k][1];
                    break;
                }
            }
            var lane = null;
            for (var l = 0; l < LANES.length; l++) {
                if (has(p, LANES[l][0])) {
                    lane = LANES[l][1];
                }
            }
            var side = healthSide(p);
            var name = text(byId(p, "name"));
            var key = [what, side, lane, name].join(":");
            var s = healths[key] || { damaged: false, dead: false, attacked: -Infinity };
            var damaged = has(p, "on_damaged");
            var dead = has(p, "is_dead");
            var fields = { what: what, side: side, lane: lane, name: name };
            if (!quiet && healths[key]) {
                if (damaged && !s.damaged && !dead && now - s.attacked >= ATTACK_GAP_MS) {
                    s.attacked = now;
                    emit("objective", merge({ state: "attacked" }, fields));
                }
                var onMap = map && map.icons.length > 0 && what !== "midboss" && what !== "shrine" && what !== "base_guardian" && what !== "boss";
                if (dead && !s.dead && !onMap && !toldRecently(objectiveKey(what, side, lane), now)) {
                    emit("objective", merge({ state: "destroyed" }, fields));
                }
            }
            s.damaged = damaged;
            s.dead = dead;
            healths[key] = s;
        }
    }

    function trackResult() {
        if (!valid(matchEnd)) {
            matchEnd = byId(root, "MatchEnd");
        }
        var shown = has(matchEnd, "ShowMatchEnd");
        if (!shown) {
            ended = false;
            return;
        }
        if (ended) {
            return;
        }
        ended = true;
        var team = hasUp(matchEnd, "LocalPlayerTeam1", 8) || hasUp(matchEnd, "localPlayerTeam1", 8) ? 1
            : hasUp(matchEnd, "LocalPlayerTeam2", 8) || hasUp(matchEnd, "localPlayerTeam2", 8) ? 2 : null;
        var winner = hasUp(matchEnd, "Team1Victory", 8) ? 1 : hasUp(matchEnd, "Team2Victory", 8) ? 2 : null;
        var result = has(matchEnd, "MatchAbandoned") ? "abandoned" : team !== null && winner !== null ? (team === winner ? "victory" : "defeat") : "unknown";
        var words = [text(byId(matchEnd, "TeamResultsTop")), text(byId(matchEnd, "TeamResultsBottom"))].filter(function (w) { return w !== null; });
        emit("result", { result: result, team: team, winner: winner, text: words.length > 0 ? words.join(" ") : null });
        summarize();
    }

    function summarize() {
        if (summarized) {
            return;
        }
        summarized = true;
        var l = me ? me.labels : null;
        var counted = me !== null && me.kills !== null;
        emit("summary", {
            name: me ? me.name : null,
            hero: me ? me.hero : null,
            hero_id: heroId,
            level: l ? count(text(l.level)) : null,
            souls: l ? text(l.souls) : null,
            objective_damage: l ? count(text(l.damage)) : null,
            kills: counted ? me.kills : tally.kills,
            deaths: counted ? me.deaths : tally.deaths,
            assists: counted ? me.assists : tally.assists,
            best_streak: bestStreak > 0 ? bestStreak : null,
            src: counted ? "kda" : "tally"
        });
    }

    function describe(entry) {
        var message = classText(entry, "messageMain");
        if (message !== null) {
            return {
                kind: "message",
                text: message,
                sub: classText(entry, "subText"),
                good: has(entry, "IsPositive") || byClass(entry, "IsPositive").length > 0
            };
        }
        var killer = classText(entry, "killerInfo");
        var victim = classText(entry, "victimInfo");
        if (killer === null && victim === null) {
            return null;
        }
        var info = ofType(entry, "CitadelHudInfoFeed", 4);
        var tags = [];
        for (var i = 0; i < FEED_TAGS.length; i++) {
            if (has(entry, FEED_TAGS[i]) || has(info, FEED_TAGS[i])) {
                tags.push(FEED_TAGS[i]);
            }
        }
        var assists = byId(entry, "AssistsContainer");
        return {
            kind: "kill",
            killer: killer,
            victim: victim,
            assists: assists ? children(assists).length : null,
            tags: tags
        };
    }

    function role(entry) {
        if (!me || !me.name) {
            return null;
        }
        if (same(entry.killer, me.name)) {
            return "killer";
        }
        return same(entry.victim, me.name) ? "victim" : null;
    }

    function report(entry) {
        if (entry.kind === "message") {
            emit("announce", { text: entry.text, sub: entry.sub, good: entry.good, src: "team" });
            return;
        }
        var mine = role(entry);
        for (var tag in FEED_OBJECTIVES) {
            if (entry.tags.indexOf(tag) >= 0) {
                var o = FEED_OBJECTIVES[tag];
                var side = entry.tags.indexOf("friendly") >= 0 ? "ours" : entry.tags.indexOf("enemy") >= 0 ? "theirs" : null;
                var onMap = tag === "bossDied" && map && map.icons.length > 0;
                if (!onMap && !(o[1] === "destroyed" && toldRecently(objectiveKey(o[0], side, null), Date.now()))) {
                    emit("objective", { state: o[1], what: o[0], side: side, killer: entry.killer, victim: entry.victim, mine: mine === "killer" });
                }
                return;
            }
        }
        recent.push({ t: Date.now(), entry: entry, me: mine, used: false });
        if (recent.length > 20) {
            recent.shift();
        }
        emit("feed", { killer: entry.killer, victim: entry.victim, assists: entry.assists, me: mine, tags: entry.tags });
        if (mine === "killer" && me && me.kills === null) {
            tally.kills++;
            emit("kill", { victim: entry.victim, src: "feed" });
        }
    }

    function trackFeed(quiet) {
        var list = children(feed);
        feedSeen = alive(feedSeen);
        for (var i = 0; i < list.length; i++) {
            if (!contains(feedSeen, list[i])) {
                feedSeen.push(list[i]);
                if (!quiet) {
                    feedWaiting.push({ panel: list[i], polls: 0 });
                }
            }
        }
        var keep = [];
        for (var j = 0; j < feedWaiting.length; j++) {
            var w = feedWaiting[j];
            var entry = valid(w.panel) ? describe(w.panel) : null;
            if (entry) {
                report(entry);
            } else if (valid(w.panel) && ++w.polls < WAIT_POLLS) {
                keep.push(w);
            }
        }
        feedWaiting = keep;
    }

    function trackToasts(quiet) {
        var found = byClass(announcements, "GenericAnnouncement");
        toastsSeen = alive(toastsSeen);
        for (var i = 0; i < found.length; i++) {
            if (!contains(toastsSeen, found[i])) {
                var title = classText(found[i], "AnnouncementTitle");
                if (title === null && !quiet) {
                    continue;
                }
                toastsSeen.push(found[i]);
                if (!quiet) {
                    emit("announce", { text: title, sub: classText(found[i], "AnnouncementDescription"), src: "toast" });
                }
            }
        }
    }

    function tick(quiet) {
        var chain = ancestors();
        root = chain[chain.length - 1];
        if (!valid(topBar)) {
            topBar = byId(root, "TopBar");
            clock = byId(topBar, "GameTime");
        }
        if (!valid(feed)) {
            feed = byId(context, "EventFeed") || context;
        }
        if (!valid(announcements)) {
            announcements = byId(root, "CitadelHudGameAnnouncements");
        }
        trackMe();
        lookForHero();
        var now = currentPhase(chain);
        if (now !== null && now !== phase) {
            phase = now;
            postgameAt = Date.now();
            if (now !== "postgame") {
                summarized = false;
            }
            emit("phase", { phase: now, hero: me ? me.hero : null, hero_id: heroId });
        }
        trackFeed(quiet);
        trackToasts(quiet);
        trackStreaks(quiet);
        trackMap(quiet);
        trackHealth(quiet);
        trackResult();
        settle();
        if (phase === "postgame" && Date.now() - postgameAt > SUMMARY_WAIT_MS) {
            summarize();
        }
    }

    function poll(quiet) {
        if (!valid(context)) {
            return;
        }
        try {
            tick(quiet);
        } catch (e) {
            var msg = String(e && e.message ? e.message : e);
            if (!errors[msg]) {
                errors[msg] = true;
                emit("error", { msg: msg });
            }
        }
        $.Schedule(POLL, function () {
            poll(false);
        });
    }

    emit("hello", { version: VERSION });
    poll(true);
})();
