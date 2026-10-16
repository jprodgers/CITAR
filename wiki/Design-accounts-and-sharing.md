# Design: accounts, sharing and the public server

**This is a design document, not a guide.** It records how the multi-user server was designed and
why, which is what you want when changing that part of CITAR. To *run* a server, start with
[DEPLOY.md](Deploying-a-server); to configure one, [CONFIGURATION.md](Configuration).

How CITAR became a multi-user server without breaking the single-user laptop workflow it had. Built
and deployed in 2026; the decisions are at the bottom.

---

## 1. What changes, and what does not

CITAR today assumes one trusted operator on a LAN. Anyone who can reach the port can read every
game, every API key reference, every report, and can reconfigure the server registry. The engine,
the bots, the benchmark scheduler and the usage ledger are all fine as they are — what is missing is
an *owner* on every object and a gate in front of every route.

So the plan is deliberately additive:

| Stays | Changes |
| --- | --- |
| Engine, rules, bots, mapgen, scenarios, probes | Every one gains an owning account |
| `saves/**` game files, replays, autosaves | Indexed by a DB row that carries owner + visibility |
| `citar/usage.py` ledger format | Keeps activities, held time and power; the work moves to the compute ledger, with a consumer and a provider, for the ratio and budgets |
| The cost model in `citar/servers.py` | Moves into the DB, per-owner, with grants and budgets |
| Vanilla-ES-module frontend, hash routing | Gains auth screens, an account menu and an admin console |

### Two modes

    CITAR_MODE=local    (default)  bound to localhost, auto-logs in a single owner account.
                                   Your current workflow is unchanged: no login, no TLS, no OAuth.
    CITAR_MODE=server              the full stack: TLS, cookies, SSO, invites, quotas, audit log.

Local mode is not "auth disabled" — it is a real account with a real session, minted automatically
because the request came from loopback and no public origin is configured. Every authorization check
in the codebase runs identically in both modes, so a permission bug cannot hide in local mode and
then appear in production. Server mode refuses to start if it is missing a secret key, a public
origin, or TLS termination.

---

## 2. Identity

### Ways in

1. **SSO** — Google, GitHub, Discord and Microsoft/Entra, via Authlib. An SSO login with a verified
   email that matches an existing account links to it rather than creating a duplicate; an
   unverified provider email never auto-links.
2. **Email + password** — argon2id, minimum length checked against a small breached-password list,
   mandatory email verification before the account can do anything but look around.
3. **Invite** — a code that carries a role, an optional email pin, a use count and an expiry. When
   the server is in invite-only mode this is the only path other than an admin creating an account.

### Anti-bot

Invite-only is a server setting an admin can flip at any time. When open signup is on, a new account
must clear all of:

- **hCaptcha** on the signup form (server-side token verification, not just the widget).
- **Per-IP and per-/24 rate limits** on signup, login, password reset and verification resend, with
  exponential backoff and a lockout that is logged.
- **Disposable-domain blocklist** for email signups, refreshed from a vendored list.
- **Email verification** before any resource can be created.
- **A probation state**: brand-new accounts can play, watch and share with other members, but cannot
  register servers, invite anyone, or publish anything to the whole internet until an admin or
  moderator clears them (configurable, and off by default for SSO accounts from providers with
  verified email).

### Roles

| Role | Can |
| --- | --- |
| **admin** | Everything. Server settings, accounts (roles, status, capabilities, limits, trust) and the audit log, invites, the global server pool, all data. |
| **moderator** | Unpublish what was published to the internet (`global` games, reports and items, and `link` ones whose link they were given). No role changes, no server settings, no access to private data, and no control of anybody's things: unpublishing is all a moderator may do to them. Suspending accounts and reading the audit log are administrators'. |
| **user** | Own games, servers, reports, benchmarks, probes and data; whatever grants they have been given. |

Roles are coarse. Fine-grained things are per-account capability flags an admin can set, plus
`invite_quota`, `max_concurrent_games` and `max_seats_per_game`:

| Capability | Lets an account | Members by default |
| --- | --- | --- |
| `create_games` | start games, launch scenarios, save maps and scenarios | yes |
| `register_servers` | add machines and share them | yes (not on probation) |
| `run_reports` | build reports | yes |
| `run_benchmarks` | save benchmark suites and start runs | yes |
| `run_probes` | save probes and run them | yes |
| `run_lab` | queue lab experiments, which run on the server's own CPU and on machines whose owners lend theirs ([Workers](Workers#lending-cpu-what-plays-on-it-and-for-whom)) | no |
| `edit_bots` | create, fork and edit bot profiles, which benchmarks are measured against | no |
| `full_llm` | seat a model that plays every turn (an LLM seat) | yes |
| `partial_llm` | use pooled prompts (a report's written analysis, a probe's offers and messages, the Servers page's test, a hybrid seat's diplomacy) | yes |
| `publish_global` | make something readable by the whole internet (`publish_public` before 0.1.6) | no |
| `invite` | mint invitation codes, up to `invite_quota` | no |

Defaults come from a server-settings template so an admin changes the policy once rather than per
user; a template saved before a capability existed falls back to the built-in default for it.
`full_llm` and `partial_llm` are the switches for an account's use of models (a future rule, say on the compute ratio,
turns them off): an LLM seat needs `full_llm` and a grant for ordinary games, every pooled prompt `partial_llm` and a
grant for pooled prompts ([Workers](Workers#when-a-machine-is-busy)).

### Profiles and the directory

Every account has a profile page, `#/u/<handle>`: its display name, a picture, a short bio (at most 1,000
characters), up to three links (`http` or `https`, a label of at most 40 characters, an address of at most
300) and the things it owns that the viewer may see (lobby games and every kind of owned item, 20 of each,
newest first). From 0.1.6's compute ledger it also shows the account's ratio. The account decides, field by
field, who sees each part, on the **Account** page's Profile card:

| Field | Shows | Default |
| --- | --- | --- |
| `profile` | the page itself: nothing on it shows to anyone who may not open it | members |
| `display_name` | the display name, wherever the account appears; otherwise its handle | members |
| `avatar` | the picture, wherever the account appears | members |
| `bio`, `links` | the bio and the links | members |
| `items` | the list of things it owns (each still follows its own sharing) | members |
| `ratio` | the compute ratio | only the account |

Each field is `private` (the account and administrators), `server` (every signed-in member) or `global`
(anyone, signed in or not). A visitor sees `global` fields, a member `server` and `global` ones, the
account and administrators everything, and a field shows only to someone who may also open the page. The
**handle always shows** wherever the account appears. An account on **probation** cannot make a field
`global` ("New accounts can show their profile to members only"), and one put on probation later is held to
members-only whatever it chose; a suspended, pending or deleted account shows nothing to anyone but itself
and administrators. The page's "View as a member" and "View as a visitor" links show it as they see it.

**Wherever an account appears** (a game's or an item's owner, the people a thing is shared with, a grant's
account, a machine's or a group's owner, the directory, the inviter an invitation names), its display name and
picture follow these fields for whoever is looking (`User.public(viewer)` in `citar/db/models.py`): a game
published to the internet names its owner to a visitor by handle unless the owner made the display name
`global`. An account's role and status are never part of it.

**The directory** (`#/users`, the nav's **People**) lists, for signed-in members, the accounts in good
standing (active or on probation) that are **listed**, newest first, 50 a page, each with what shows to the
viewer. Its search matches a handle, and a display name only where the searcher may see it, so a hidden name is
never found by searching for it. "List me in the members' directory" off takes the account out of the
directory only: its page still opens for whoever may see it.

**The picture.** A PNG, JPEG or WebP file of at most 256 KB and 1024x1024 pixels, uploaded on the Profile card.
It is checked by its header alone (`citar/auth/images.py`): the magic bytes, the structure walked to its end (a
cut-short or damaged file is refused) and the size read from the header; nothing is decoded and no image library
is involved. SVG, GIF, HTML and anything else are refused with the rule. It is stored as
`<data dir>/avatars/<user id>-<hash>.<ext>` (never beside the code or the saves; the previous one is removed)
**without its metadata**: a picture goes to everyone its field reaches, so the Exif a phone or a camera writes
(where and when the photo was taken, the camera's serial number), XMP, IPTC, PNG text chunks and comments are
left out, and so is anything after a JPEG's end. Only whole segments or chunks are dropped, nothing is decoded or
re-encoded: the pixels are the upload's, and what changes how it looks (an ICC colour profile, gamma, the pixel
size, an animation) stays. It is served from `/api/users/<handle>/avatar` with the type its header proved,
`nosniff` and a policy that lets it load and run nothing, to whoever its field shows to. Without an upload, a
sign-in provider's picture shows, under the same field.

### Administering accounts

Administrators manage accounts on the **Users** page (`#/admin/users`, beside **Server** in the nav): every
account, searchable by handle, name or email and filtered by role and status, and a drawer per account for its
**role**, **status** (a suspension needs a reason, which the account is told when it signs in; suspending signs
it out everywhere), **capabilities** (each the role's default, or overridden on or off), **limits**
(`invite_quota` 0 to 1,000, 0 being none; `max_concurrent_games` 1 to 100, since the `create_games` capability is
what stops an account starting any; `max_seats_per_game` 0 to 64, 0 being the ruleset's most), **trust tier**
(`new`, `provisional`, `trusted`, `quarantined`: set by hand in 0.1.6) and **directory listing**, with the
account's audit rows beneath. Each save is one audit row naming the fields it changed with their old and new
values. The server never loses its last active administrator: that account cannot demote itself, suspend itself
or put itself on probation. The page's **Audit log** reads the whole log, newest first, filtered by who did it (a
handle, or `admin_cli` for the command line's changes), the action (or a prefix: `user.*`) and the object, 100 a
page. For anyone but an administrator these pages, and their routes, do not exist (404).

The command line does the same on the server's database (`citar-admin`, `python -m citar.server.admin_cli`):
`set-role`, `set-status` (`--reason` for a suspension), `set-caps NAME run_lab=on publish_global=default`,
`set-limits NAME --games 4 --seats 8 --invites 2`, `set-trust NAME trusted` and `audit [--actor] [--action]
[--object-type] [--object-id] [--before] [--json]`, under the same rules, each change recorded as `admin_cli`'s.

---

## 3. Authorization

One module, `citar/auth/access.py`, answers a single question:

    access.on(session, viewer, object) -> set of {"view", "play", "manage", "moderate", "admin"}

Every route asks it. Nothing hand-rolls a check. A caller who may not even see the object gets 404,
never 403: a 403 would confirm that it exists.

### Visibility

Each shareable object (game, owned item, server group) carries:

- `visibility`, on a ladder of five levels (0.1.6):

  | Level | Who may view | Listed for |
  | --- | --- | --- |
  | `private` | the owner and admins | the owner and admins |
  | `allowlist` | the owner, admins and the accounts with an ACL entry | the same |
  | `link` | the same, and anyone presenting the link's key | nobody else: unlisted |
  | `server` | the same, and every signed-in member in good standing (active or on probation) | members |
  | `global` | anyone on the internet, no account needed | everyone |

- an **ACL** of `(user, permission)` entries, which count at every level, so sharing with one person
  never needs a wider level;
- a secret slug for `link`: unguessable, revocable, and rotating it kills every old link.

Making something `global` needs the `publish_global` capability (members do not have it by default,
and probation never does); `server` needs nothing, since it stays among members. The levels live in
one tuple, `access.VISIBILITY`, with one validator, `access.check_visibility`; the old `public` level
is gone (it is `global`), and a test scans the code for any visibility string off the ladder.

Games separate **play** from **watch**, so a private game between two people can still have a
spectator link. Reports and collected data use the same levels, so a user can publish an analysis
while the underlying game stays private.

`link` objects are readable by anyone holding the URL but never listed or indexed
(`X-Robots-Tag: noindex`). The share URL offered to the owner carries the key for `link`, and is the
object's plain address for `server` and `global`.

### Sharing in the web client

Every shareable thing has a **Share** button where it is listed, for whoever manages it: games in the
lobby, reports, benchmark runs, probe runs, scenarios, maps and bot profiles. Its dialog sets the
visibility on the ladder above (the `global` choice only for accounts with `publish_global`), shows the
share link with **Copy** and, for `link`, **New link** (rotating the key, so every old copy stops
working), lists the people it is shared with by handle and what each may do (view, play for a game,
manage), adds one by handle and removes one. A game's dialog also holds its pooling choice; a report's
gives the page for people with no account (below). If a moderator took the thing down, the dialog says
so and why. A moderator who does not manage something sees an **Unpublish** button instead, on what is
published to the internet or a link they were given, which asks for the reason its owner will read.

Share links open where they point:

- a game's link (`#/game/<id>`, with `?k=<key>` for `link`) opens the game screen with no seat token:
  the account, or the key, is the credential, and the key travels with the screen's own links (the
  recap, looking through one civilization's eyes) and on its live socket. A game with a person playing
  in it can be watched only once it is over, as before;
- a report's link (`#/reports/<id>`) opens the report, its frame and its download carrying the key;
- the other kinds' links open their page, whose list shows the item the key opens to whoever holds the
  key (and only them: it stays unlisted for everyone else);
- these pages open without signing in, so a visitor with a `global` thing's link, or a `link`'s key,
  sees it. Anything the viewer may not see is the same **Not found** page as something that does not
  exist, offering a visitor the sign-in.

In the lobby a game shared with you shows "shared with you" and can be watched and replayed; its seats,
playing it and closing it are its managers'.

### Reports

A report is an owned item like the others, and what it **gathers** follows its owner
(`citar/reports/reach.py`; [Reports](Servers-costs-and-reports#what-a-report-contains)):

- a member's report covers the activities of what its owner may see: their own games, runs, probe
  runs, experiments and reports, and those shared with them, with members or with everyone. Another
  member's private game, run or probe run is not in it, not even by name or id;
- it prices **only the owner's own machines**: an activity that also ran on the server's machines or
  on another member's keeps its outcome and its usage, but not what it cost there (a note in the
  report says how many did), and no other machine's configuration, tariff, depreciation or hardware
  appears, nor a pricing note about one;
- figures wider than that come only as **pooled averages** (the report's "Data" choice: what you may
  see, plus pooled averages, or pooled averages only), per model, over the games other accounts pool,
  each withheld until enough accounts are behind it;
- an administrator's report sees everything, as before;
- an owner not in good standing when it is built (suspended, say; an administrator too) reaches only
  what is published to everyone, with no machine priced;
- whoever a report is shared with reads the report; of its specification they get only the items and
  machines of its scope they may see and whether it has a written analysis (not its instructions or
  the machine and model that wrote it), and of a failed build its error, not its traceback.

A report shared by `link` or published `global` also has a page for people with no account:
`/r/<key>` for a `link` report (its key, never its id), `/r/<id>` (or its key) for a `global` one, and
**404** for anything else, its owner's own private report included (who reads it in the app). The page
is the report's HTML and nothing else, `noindex`, with a policy that lets it load nothing.

### Owned items

Every job a member can start is an **owned item**, with an owner and a visibility on the ladder
above, and the same sharing as a game: benchmark suites and runs, probes and probe runs, lab
experiments, scenarios, maps, bot profiles and reports. Their files stay where they always were (the
benchmark folder, `saves/probes/`, `saves/scenarios/`, `saves/maps/`, `saves/bots/profiles/`,
`saves/lab/`, `saves/reports/`); a row in `owned_items` (`citar/items.py`) says whose each one is. The
rules:

- **Making one** needs the kind's capability (above): a suite or a run `run_benchmarks`, a probe or a
  probe run `run_probes`, an experiment `run_lab`, a profile `edit_bots`, a map or a scenario
  `create_games`, a report `run_reports`. It is the maker's, `private`. Work that runs on a machine
  (a benchmark run, a probe run, a report's written analysis) is admitted to each machine it names,
  as a game's seats are: the member's own machine, or one of a group they hold a grant on.
- **Reading one** needs view: its owner, administrators, its allowlist, and members or the whole
  internet as its visibility says. Listings show only what the caller may see; a `link` item is never
  listed.
- **Changing one** (saving over it, deleting it, pausing or cancelling a run, stopping a probe run,
  rerunning a report, its queue priority, its sharing) needs manage: its owner, an ACL manager or an
  administrator. Saving under an id that is somebody else's is refused before anything is written.
- **Using one** in something else (a map in a game, a scenario in a probe, a bot profile in a seat or
  an experiment, a suite in a run) needs view on it, when the thing is run or launched as well as
  when it is saved: a probe shared wider than its scenario runs only for those who may see the
  scenario, and a scenario's own seats' profiles are checked at its launch.
- A **benchmark run's games** are its owner's, at the run's visibility, and follow it when its
  visibility changes or it is taken down. A probe case opened to be looked at is the run's owner's.
- A suite's machines and a probe's subject **name** machines and never describe them (no base URL,
  key or key variable), as a seat's model does (see [AI players](AI-players)); a report's
  written analysis names a machine its maker may use.
- The **shipped bot profiles** (Standard, Classic production, Idle) are the server's: `global`, and
  only administrators manage them (they cannot be changed, only forked).
- A file of one of these kinds with **no row** is nobody's, and every route answers 404 for it,
  administrators included. Such files are left by a crash between writing the file and its row, by a
  file copied in by hand, by `citar lab submit` where no server database is at hand, and by everything
  from before 0.1.6. They are **adopted** at every start that has an administrator, and when the first
  administrator is made on a fresh server (`citar admin create-admin`, `set-role`, the setup wizard, an
  invitation with the admin role): each goes to the account its file names (a run's or a report's
  owner), else to the first administrator (in local mode, the local owner), `private`, with one audit
  row for the lot. Games in the index that nobody owns are adopted the same way. A file that does not
  read (cut short, corrupt) is adopted under its id by that administrator, who can remove it, and is
  logged; nothing adoption meets stops the server's start or the making of an administrator (a failed
  adoption is rolled back alone and tried again at the next start).

The sharing routes are the same for every kind: `/api/items/{kind}/{ref}/sharing` (see the
[API](HTTP-and-tool-API#owned-items)).

### Moderation

A moderator's only extra permission is **moderate**: on a `global` object, and on a `link` object
presented with its key, a moderator may view it and **unpublish** it
(`POST /api/games/{gid}/unpublish` or `POST /api/items/{kind}/{ref}/unpublish`, with a reason). Unpublishing makes the object private, rotates
its slug so every shared link dies, and writes an audit row with the reason. The owner sees the
reason in the item's sharing panel, and that a moderator (or an administrator) took it down, never
which one: administrators have the audit log. A benchmark run taken down takes its games down with
it. Every control (pause, seats, sharing, delete, stop, rerun) needs **manage**,
which only the owner, an ACL manager and administrators have, so a moderator who has taken something
down has no further way in. On a `server` object a moderator is a member like any other; below `link`,
nothing.

### The games index

Each game has a row in `games` (`citar/server/ownership.py`): its owner, visibility, data sharing, turn
and status. The status is `playing` while the game is played, `finished` once it is over, `closed`
when its session stopped before the end (the lobby's **End game**), and `playing` again when it is
loaded. The session's save writer keeps it, after each round's autosave is on the disk, and so does
the session's stop: never under the game's lock. Only lobby games in `setup` or `playing` count
against an account's `max_concurrent_games`; benchmark and probe games are limited by their own
queues. At boot, a row still `playing` whose game did not come back is closed. Closing a game keeps
its saves its owner's; deleting every save of it deletes the game. A game loaded without a row (its
database was set aside, or a probe's case opened to be looked at) gets one, owned by nobody, private
and pooling nothing (only administrators reach it); the administrator who loads such a save from the
Saves list owns it from then on, with their own pooling default.

### Data pooling

Every account has `data_sharing`: `pool` (default) or `private`, and so does each game (a new game
takes its owner's setting; its sharing dialog changes it).

- **pool** — the game's metrics feed the pooled figures other members see.
- **private** — the data is excluded from every pooled figure; its owner still sees it in their own
  figures and reports.

A game **pools** when its own setting is `pool` and its owner's account is `pool` too: opting the
account out withdraws every one of its games, the ones played before included, and opting back in
returns those whose own setting allows it. The filter is applied when a figure is made
(`aggregate.scope_for`), never when a game is recorded.

The **Models** page (`/api/models/scores`) and its comparison (`/api/metrics/compare`) are made, for a
member, from their own games and the pooled ones. Other members' games are counted (`pooled_games`),
never named. A model's pooled part is shown only when at least `aggregate_min_contributors` (3 by
default, a console setting) other accounts are behind it; below that it is withheld, with how many
there were, and the member sees their own games' figures alone. Counting the other accounts only
matters: a figure made of your games and one other person's is that person's figure with yours
subtracted. An administrator's figures cover every game.

---

## 4. Servers, grants and budgets

This is the heart of the feature and the biggest change to existing code. Every machine is a row of the
`servers` table: the host, the online APIs and LAN machines an administrator registers (the server dials them), and
the members' machines behind the CITAR helper. A pre-0.1.6 `config/servers.json` is imported once, under its own ids,
owned by the first administrator, then renamed ([Configuration](Configuration#the-machines-in-the-database-not-a-file)).
Who may do what to a machine, and how a revoked token or a disabled machine drops its helper, is in
[Workers](Workers#who-may-do-what-to-a-machine).

### Ownership and groups

A **server** belongs to one account. Servers are placed in **server groups**, which are the unit of
sharing — "Home GPUs", "General pool", "Overnight scenario box". A group has a visibility and a
list of **grants**.

A **grant** gives a subject (a user, or everyone) permission to use a group, optionally narrowed by:

- **purpose** — normal games (a model seat that plays every turn), pooled prompts (a report's written analysis, a
  probe's offers and messages, a machine test, a hybrid seat's diplomacy), benchmarks, probes, scenarios, and **Lab
  (CPU work)**: the lab's games played on the machines' CPUs ([Workers](Workers#lending-cpu-what-plays-on-it-and-for-whom)).
  Nothing ticked means every kind of model work; the CPU is lent only by a grant that ticks Lab (CPU work)
- **window** — which hours, which weekdays
- **budget** — a spend cap per period
- **concurrency** — how many of the subject's requests may run on one of the group's machines at once, its seats' and
  its pooled prompts' together (0: no cap below the machine's slots)
- **expiry**

So the two examples from the brief are both just grants:

    General pool:     grant to everyone, all purposes, all hours, no budget, 2 concurrent games.
    Overnight box:    grant to everyone, purpose=scenarios, 00:00–06:00 owner-local, 100,000 credits/month.

### Time zones

Every account stores an IANA time zone (detected from the browser on first login, editable). Windows
are stored as `(weekday, start_minute, end_minute)` **in the owning account's zone** and converted
with `zoneinfo` at evaluation time, so DST is handled and a "midnight to 6am" window stays at the
owner's midnight year-round. The UI always shows both readings:

    00:00–06:00 America/New_York  ·  your 05:00–11:00 Europe/London

### Budgets

Budgets are **accounting and admission control, not payments.** No money moves; nobody is charged.
A budget is a cap that stops work from being scheduled.

A grant's budget is counted in **credits**, the compute ledger's unit ([below](#the-compute-ledger-and-the-ratio)),
per day, week, month or in total, in the machine owner's time zone (a "month" ends at their midnight), and it is
**hard** or **soft**. It is kept per request:

- **Reserve.** When a request of work admitted under the grant (a seat's step, a pooled prompt) is given its slot on
  the machine, it reserves an estimate: the machine's mean credits per step for that model (at least 1), or on an
  online API the price of the request's `max_tokens` of output plus its input, times the API class's rate. If a hard
  budget's spent plus reserved would pass its amount, the request is **refused** with the period's end as when to try
  again: a lobby game **pauses** with the budget's reason (as for quiet hours) and resumes by itself when the period
  turns (a game paused for its budget when the server restarts comes back running, and pauses again at its seat's
  next request while the budget is still spent); a pooled prompt is refused and its caller falls back (a report is
  written without its analysis, a probe's offer or message case records the refusal as its error); a probe's turn case
  ends with `budget` as its end reason. A hard budget on an online API whose model has no price refuses every request
  ("this model has no price, so its budget cannot be kept") rather than letting unpriced work through at 0.
- **The grant, not the budget it had.** A game's seats and a pooled prompt are bound to the grant that admitted them,
  and each request is held to the budget that grant has when the request runs: a budget added to a grant, changed,
  replaced or removed holds the work already running under it from its next request.
- **A lab game** played on a member's machine under a grant that lends its CPU reserves the game's expected credits (its
  reference cost at the machine's CPU class's rate, at least 1) when the server picks it for the machine; a hard
  budget with no room for it holds that member's games back from the machine (they wait, and nothing is counted
  against the machine that would have played them), and its credits are settled when its result is taken, a game
  still playing when the server restarted included.
- **Settle.** Each step's credits, priced as the step ends, are counted against the budget at once, and written to the
  budget's entry for that activity and period (with the step's event); the reservation goes.
- **Soft** budgets only warn: the request that takes one past its amount is let through, the game's log says so (once
  a period), an audit row records it, and the machine owner's Servers page shows the meter over its amount.
- `refresh_from_ledger` (in `citar/pool/budgets.py`; `citar admin refresh-budgets [--period KEY]`) rebuilds a
  period's entries from the ledger's events: an administrator's repair, when an entry was lost or the credits
  repriced. A period key names one kind of period (`2026-10-12` a day, `2026-W41` a week, `2026-10` a month, `all` a
  total), and only the budgets counted in that kind are rebuilt. A running server reads the rebuilt entries at its
  ledger's next rollup pass, within five minutes.

The machine's owner and administrators are not held to grants, so their own work on the machine has no budget.

### The compute ledger and the ratio

Every model step on a machine and every second of the host's CPU spent on somebody's work is one row of the **compute
ledger** (`compute_events`; `citar/ledger.py`): the machine, the activity it served (a game, a benchmark run's game,
a probe run, a report, a lab game, a machine test), its **consumer** (the activity's owner) and its **provider** (the
machine's owner), the generation seconds or CPU core-seconds, the tokens, and the **credits** they are worth, priced
when the event is written from the machine's hardware class ([Configuration](Configuration#credits-and-hardware-classes)).

- The consumer is null for the server's own data collection (an item an administrator marked with
  `citar admin set-collection`), whose compute counts as given by whoever's machine served it and used by nobody.
- The provider is null for a machine the site provides (the host, an online API an administrator registered).
- **Self-use** (the same member on both sides, or the server on both) counts on **neither** side: a member's own games
  on their own GPU are no gift and no use. Another member's game or pooled prompt on your machine is **given**; your
  games on the server's CPU, or on somebody else's GPU, are **used**.
- Generation time is the smaller of what a helper reports and what the server observed (a modified helper cannot
  raise its owner's credits past the time the server waited for it).
- The host's CPU is one event per activity for each window it was measured over (about a minute; the event's
  `observed_s`), and a lab game's one event for the whole game (`observed_s` its wall time), which costing spreads
  the CPU seconds over.
- The **ratio** is given credits over used credits ("only given" when nothing was used).

Who sees it: your own, by machine and by kind of work, on the **Account** page's Compute card
(`GET /api/ledger/me?range=24h|7d|30d|all`); anyone else's when its `ratio` profile field shows to you (private by
default, so the account and administrators; `GET /api/users/{handle}/ratio`), and administrators always, on the
admin users page's 30-day column. A profile shows the ratio line under the same rule. Totals read hourly rollups for
complete hours and the raw events for the rest, so they are current to the event. Nothing restricts anyone by their
ratio in 0.1.6; a later admission rule may.

### Admission control

When a game is launched or a seat changed, each model seat is admitted by its task class (a seat that plays every
turn needs `full_llm` and a grant for ordinary games; a hybrid seat `partial_llm` and one for pooled prompts), and each
pooled prompt is admitted when it is made. The server checks, in order: grant exists → purpose allowed → inside an
availability window → budget remaining → a worker is actually online. Each failure produces a specific, quotable
reason ("Overnight box is available 00:00–06:00 owner time; next window opens in 4h 12m"), never a generic denial.
A busy machine is not a refusal: admission decides who may use a machine, the prompt scheduler when, so the work
waits its turn, and the grant's concurrency is the scheduler's cap on the subject's requests there.

---

## 5. The worker agent

The VPS cannot reach a GPU sitting behind your home NAT, so the connection is inverted.

The CITAR helper (`citar-helper`; the Python worker before 0.1.6) is a small program you run on the
machine that has the model. It dials **out** to `wss://citar.example.com/ws/worker`, authenticates
with a per-server token and the machine's key, and then serves inference requests over that one
connection (protocol 2, `crates/citar-helper/PROTOCOL.md`).

    laptop / desktop (LM Studio, Ollama)          VPS
        citar-helper  ── outbound wss ───────────▶  worker hub
                      ◀── inference requests ───
                      ─── raw completions ─────▶

Properties that matter:

- **No inbound ports.** Nothing on your home network is exposed.
- **Keys stay home.** If a worker fronts a paid API, the API key lives on the worker machine and is
  never uploaded. The VPS knows only that the server exists and what it costs per token.
- **Revocable.** Each worker token maps to one server record and can be rotated or killed from the UI.
- **Honest availability.** A server is "online" only while its worker holds a live connection with
  recent heartbeats; restricted hours and budget exhaustion take it out of the pool cleanly, and
  in-flight requests are drained rather than cut.
- **Bounded.** The worker enforces its own concurrency limit and rejects work outside its windows,
  so a VPS bug can never run your GPU at 3am — the owner's machine has the final say.

The protocol is versioned JSON frames over one websocket: `hello` / `welcome` / `catalog` /
`request` / `chunk` / `done` / `error` / `ping`. The existing provider adapters in
`citar/agents/providers/` get a sibling `worker` provider, so the engine is unaware of the difference
between a local model and one three hops away.

Games themselves run on the VPS. Worker-hosted game sessions are a later, additive change; the job
queue boundary is defined now so that it does not become a rewrite.

---

## 6. Storage

SQLite in WAL mode via SQLAlchemy 2.0, with Alembic migrations, and the Postgres path written and
tested from day one so moving is a connection-string change. Game saves stay as files on disk,
referenced by a DB row that carries owner, visibility and ACL.

Tables (`citar/db/models.py`): `users`, `identities`, `auth_sessions`, `invites`, `email_tokens`,
`servers`, `server_groups`, `access_grants`, `availability_windows`, `budgets`, `budget_entries`,
`worker_tokens`, `hardware_classes`, `games`, `owned_items`, `acl_entries`, `compute_events`,
`compute_rollups`, `cpu_jobs`, `cpu_job_runs`, `job_costs`, `audit_log`, `rate_limits`, `settings_kv`. A report
is an owned item of kind `report`; deleting an account leaves its machines and groups ownerless (managed by
admins), never deletes them.

**The 0.1.6 baseline.** 0.1.6 starts the schema afresh: one revision, `0016a_baseline`, holds every
table and column the 0.1.6 features need (profiles, owned items, the compute ledger, machine settings),
so later changes add behaviour rather than migrations; a change that cannot is a revision on top of it
(`0016b_compute_cache_tokens`: the ledger keeps a step's cached input tokens, which an online API prices
apart; `0016c_helper_jobs`: the CPU job store, one row per game a helper or the server plays for the lab, per
run of it and per kind of job's reference cost, and on each machine the key its helper registered and its
proof-of-work counters). A database from 0.1.5 or an earlier 0.1.6 build
(revisions `f064316a6b34` and `767c824c2fcb`) is **not migrated**:

- on SQLite, the first start of 0.1.6 writes the database's log into its file, lets go of it, and
  **renames** it (with any `-wal` and `-shm` beside it) to `citar-0.1.5-<YYYYmmdd-HHMMSS>.db` in the
  same folder, then starts on a fresh database. Nothing is deleted, and the boot's warning names the
  new file. If the rename fails (on Windows, another process has the file open: another CITAR, a
  database browser, a backup or sync tool), the renames already made are undone and the server
  refuses to start, naming the file: it never runs on the old database, or on half of it renamed;
- on Postgres, the server refuses to start: create a new database and point `CITAR_DB_URL` at it, or
  drop the old schema;
- a database at a revision this version does not know (a newer version's) refuses the boot, naming
  the revision.

Accounts, servers, grants and sharing start empty. Saved games stay on disk and are indexed again as
they are loaded (a game restored at start, or loaded from the Saves list by an administrator, gets its
row back, owned by whoever loads it).

---

## 7. Security posture

- argon2id password hashing; no password is ever logged, echoed or included in an error.
- Session cookies: `HttpOnly`, `Secure`, `SameSite=Lax`, opaque server-side session ids, absolute and
  idle expiry, revocable from an account's device list.
- CSRF: double-submit token required on every state-changing request; the WS upgrade checks `Origin`.
- TLS terminated by Caddy with automatic Let's Encrypt certs; HTTP redirects to HTTPS. **HTTPS is
  required** — OAuth providers will not accept plain-HTTP callbacks and `Secure` cookies need it.
- Security headers: HSTS, CSP (no inline script — the frontend is already ES modules),
  `X-Content-Type-Options`, `Referrer-Policy`, frame-ancestors none.
- Secrets from environment only. Nothing sensitive in the project folder — it syncs to OneDrive.
- Rate limits on every unauthenticated route, and per-account limits on expensive ones.
- An append-only audit log for logins, role changes, grants, suspensions, publishes and deletions.
- The seat tokens that exist today stay, but become scoped to a game and revocable, and no longer
  substitute for an account.

---

## 8. Build order

Each phase ends green (full test suite) and committed.

0. Foundations — settings module, DB layer, migrations, both modes booting.
1. Identity — users, sessions, password + email, four SSO providers, invites, hCaptcha, roles.
2. Authorization — ownership, ACLs, visibility, the single `permissions()` gate on every route.
3. Servers — ownership, groups, grants, windows in owner time, budgets, admission control.
4. Worker agent — protocol, hub, the worker (since 0.1.6 the CITAR helper), the `worker` provider, live test laptop→VPS.
5. Sharing — public/link games and reports, spectators, data pooling and private analysis.
6. Web UI — auth screens, account settings, sharing dialogs, admin console, moderation queue.
7. Hardening and deploy — rate limits, audit log, security review, Caddy + systemd, load test, docs.

---

## 9. Decisions

| Question | Decision |
| --- | --- |
| Reaching home GPUs | Outbound worker agent; no inbound ports at home |
| Storage | SQLite (WAL) now, Postgres path built and tested alongside |
| Email | Existing SMTP on `example.com` |
| Deployment | SSH to the VPS; deployed and tested against the real domain |
| SSO providers | Google, GitHub, Discord, Microsoft/Entra |
| Anti-bot | hCaptcha, plus rate limits, verification, disposable-domain blocking, probation |
| Game compute | VPS runs games; workers serve models only, with the queue boundary left clean |
| Local workflow | Single-user local mode, auto-login, existing data migrated to that account |
| Budgets | Accounting and admission control only, in credits — no payment processing anywhere |


---

*This page is generated from [`docs/server/ACCOUNTS.md`](https://github.com/jprodgers/CITAR/blob/main/docs/server/ACCOUNTS.md) and any edit made here will be overwritten. Corrections are welcome as a pull request.*
