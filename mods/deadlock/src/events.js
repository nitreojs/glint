(function () {
    "use strict";

    var VERSION = "1.0.0";
    var TAG = "[AFTERGLOW]";
    var POLL = 0.1;
    var WAIT_POLLS = 10;
    var DEATH_GAP_MS = 5000;
    var FEED_TAGS = [
        "enemyDied", "teammateDied", "killedSelf", "noKiller", "hasAssists", "has_killstreak", "friendly", "enemy",
        "bossDied", "midBoss", "IdolPickedUp", "IdolReturned", "KothCompleted", "SoulBagPickup", "SoulBagStolen",
        "isRejuvenatorPickup", "isRejuvenatorUsed", "is_item"
    ];
    var OBJECTIVE_TAGS = ["bossDied", "midBoss", "IdolPickedUp", "IdolReturned", "KothCompleted", "isRejuvenatorPickup", "isRejuvenatorUsed"];
    var PHASES = [["GameStatePostGame", "postgame"], ["GameStatePreGameWait", "pregame_wait"], ["GameStatePreGame", "pregame"]];

    var context = $.GetContextPanel();
    var root = null;
    var topBar = null;
    var feed = null;
    var announcements = null;
    var clock = null;
    var me = null;
    var phase = null;
    var feedSeen = [];
    var feedWaiting = [];
    var recent = [];
    var pending = [];
    var toastsSeen = [];
    var errors = {};
    var lastDeath = 0;

    function emit(event, fields) {
        var out = { v: 1, event: event };
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

    function text(p) {
        try {
            if (!valid(p) || typeof p.text !== "string") {
                return null;
            }
            var t = p.text.replace(/<[^>]*>/g, "").replace(/\s+/g, " ").trim();
            return t === "" ? null : t;
        } catch (e) {
            return null;
        }
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

    function classText(p, c) {
        var found = byClass(p, c);
        return found.length > 0 ? text(found[0]) : null;
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
            try {
                p = p.GetParent();
            } catch (e) {
                p = null;
            }
        }
        return out;
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

    function firstOf(p, c) {
        var found = byClass(p, c);
        return found.length > 0 ? found[0] : null;
    }

    function labelsOf(panel) {
        return {
            name: firstOf(panel, "PlayerName"),
            hero: firstOf(panel, "HeroName"),
            kills: firstOf(panel, "kills"),
            deaths: firstOf(panel, "deaths"),
            assists: firstOf(panel, "assists")
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
                queue("kill", { kills: k, src: "kda" }, "victim");
            }
        }
        if (counted && me.assists !== null && now.assists > me.assists) {
            for (var a = me.assists + 1; a <= now.assists; a++) {
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
        if (entry.killer === me.name) {
            return "killer";
        }
        return entry.victim === me.name ? "victim" : null;
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

    function report(entry) {
        if (entry.kind === "message") {
            emit("objective", { text: entry.text, sub: entry.sub, good: entry.good });
            return;
        }
        var mine = role(entry);
        for (var i = 0; i < OBJECTIVE_TAGS.length; i++) {
            if (entry.tags.indexOf(OBJECTIVE_TAGS[i]) >= 0) {
                emit("objective", { what: OBJECTIVE_TAGS[i], killer: entry.killer, victim: entry.victim, mine: mine === "killer" });
                return;
            }
        }
        recent.push({ t: Date.now(), entry: entry, me: mine, used: false });
        if (recent.length > 20) {
            recent.shift();
        }
        emit("feed", { killer: entry.killer, victim: entry.victim, assists: entry.assists, me: mine, tags: entry.tags });
        if (mine === "killer" && me && me.kills === null) {
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
                    emit("objective", { text: title, sub: classText(found[i], "AnnouncementDescription"), src: "announce" });
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
        var now = currentPhase(chain);
        if (now !== null && now !== phase) {
            phase = now;
            emit("phase", { phase: now, hero: me ? me.hero : null });
        }
        trackFeed(quiet);
        trackToasts(quiet);
        settle();
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
