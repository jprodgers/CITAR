# Design: accounts, sharing and the public server

**This is a design document, not a guide.** It records how the multi-user server was designed and
why, which is what you want when changing that part of CITAR. To *run* a server, start with
[DEPLOY.md](DEPLOY.md); to configure one, [CONFIGURATION.md](../CONFIGURATION.md).

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
| `citar/usage.py` ledger format | Rows gain an `owner` and a `server owner`, for budgets |
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
- **A probation state**: brand-new accounts can play and watch but cannot register servers, invite
  anyone, or publish anything publicly until an admin or moderator clears them (configurable, and
  off by default for SSO accounts from providers with verified email).

### Roles

| Role | Can |
| --- | --- |
| **admin** | Everything. Server settings, user roles, invites, suspend/delete, global server pool, all data. |
| **moderator** | Review and act on reported content, suspend accounts, unpublish public games/reports, read the audit log. No role changes, no server settings, no access to private data that was not reported. |
| **user** | Own games, servers, reports and data; whatever grants they have been given. |

Roles are coarse. Fine-grained things are per-account capability flags an admin can set:
`can_register_servers`, `can_run_reports`, `can_publish_public`, `invite_quota`,
`max_concurrent_games`, `max_seats_per_game`. Defaults come from a server-settings template so an
admin changes the policy once rather than per user.

---

## 3. Authorization

One module, `citar/auth/access.py`, answers a single question:

    permissions(viewer, object) -> set of {"view", "play", "manage", "admin"}

Every route asks it. Nothing hand-rolls a check.

### Visibility

Each shareable object (game, report, dataset, server group) carries:

- `visibility`: `private` | `allowlist` | `link` | `public`
- an **ACL** of `(user, permission)` entries for `allowlist`
- a secret slug for `link` — unguessable, revocable, and rotating it kills every old link

Games separate **play** from **watch**, so a private game between two people can still have a public
spectator link. Reports and collected data use the same four levels, so a user can publish an
analysis while the underlying game stays private.

`public` means listed and readable by anyone on the internet, no account required. `link` means
readable by anyone holding the URL but never listed or indexed (`X-Robots-Tag: noindex`).

### Data pooling

Every account has `data_sharing`: `pool` (default) or `private`, overridable per game.

- **pool** — the game's metrics feed the server-wide aggregates, model scorecards and averages.
- **private** — the data is excluded from every aggregate and only the owner (and whoever they grant)
  can analyse it. Requires the `can_run_reports` capability to be useful, which is where private
  analysis lives.

The aggregation layer filters on this flag at query time, not at write time, so a user flipping from
private to pooled retroactively contributes and flipping back retroactively withdraws. Aggregates
carry a minimum-contributor threshold so a public average can never be reversed into one private
user's numbers.

---

## 4. Servers, grants and budgets

This is the heart of the feature and the biggest change to existing code. `config/servers.json`
becomes a per-owner table; the file stays supported as an import/export format.

### Ownership and groups

A **server** belongs to one account. Servers are placed in **server groups**, which are the unit of
sharing — "Home GPUs", "General pool", "Overnight scenario box". A group has a visibility and a
list of **grants**.

A **grant** gives a subject (a user, or everyone) permission to use a group, optionally narrowed by:

- **purpose** — normal games, benchmarks, probes, scenarios, lab runs
- **window** — which hours, which weekdays
- **budget** — a spend cap per period
- **concurrency** — how many simultaneous games may use the group
- **expiry**

So the two examples from the brief are both just grants:

    General pool:     grant to everyone, all purposes, all hours, no budget, 2 concurrent games.
    Overnight box:    grant to everyone, purpose=scenarios, 00:00–06:00 owner-local, $100/month.

### Time zones

Every account stores an IANA time zone (detected from the browser on first login, editable). Windows
are stored as `(weekday, start_minute, end_minute)` **in the owning account's zone** and converted
with `zoneinfo` at evaluation time, so DST is handled and a "midnight to 6am" window stays at the
owner's midnight year-round. The UI always shows both readings:

    00:00–06:00 America/New_York  ·  your 05:00–11:00 Europe/London

### Budgets

Budgets are **accounting and admission control, not payments.** No money moves; nobody is charged.
A budget is a cap that stops work from being scheduled.

Spend comes from the existing usage ledger priced by `citar/costing.py`, which already handles
electricity, depreciation, lease costs, hourly rates and per-token API prices. New work:

- ledger rows gain `owner` (who ran it) and `server_owner` (whose hardware paid for it)
- a rolling **budget meter** per (grant, period) that a game must reserve against before it starts
- reservations, so a long game cannot silently blow past a cap that was nearly exhausted at launch
- soft caps (warn, keep going) and hard caps (refuse to start, pause running work at the boundary)

### Admission control

When a game is launched, for each LLM seat the server resolves the chosen server group and checks,
in order: grant exists → purpose allowed → inside an availability window → budget remaining →
concurrency slot free → a worker is actually online. Each failure produces a specific, quotable
reason in the lobby ("Overnight box is available 00:00–06:00 owner time; next window opens in 4h
12m"), never a generic denial. Failures that are only about timing offer to queue the game instead.

---

## 5. The worker agent

The VPS cannot reach a GPU sitting behind your home NAT, so the connection is inverted.

`citar-worker` is a small program you run on the machine that has the model. It dials **out** to
`wss://citar.example.com/ws/worker`, authenticates with a per-server token, and then serves
inference requests over that one connection.

    laptop / desktop (LM Studio, Ollama)          VPS
        citar-worker  ── outbound wss ───────────▶  worker hub
                      ◀── inference requests ───
                      ─── streamed responses ──▶

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

Tables: `user`, `identity`, `session`, `invite`, `email_token`, `capability`, `server`,
`server_group`, `group_member`, `grant`, `availability_window`, `budget`, `budget_reservation`,
`game`, `acl_entry`, `report`, `dataset`, `audit_log`, `rate_limit`, `worker_token`, `setting`.

**Migration.** `scripts/migrate_to_accounts.py` creates the first admin account, imports
`config/servers.json` into the tables as that account's servers, and assigns every existing save,
report, benchmark run and probe run to it. It is idempotent and takes a backup first. Nothing you
have is lost or reset.

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
4. Worker agent — protocol, hub, `citar-worker`, the `worker` provider, live test laptop→VPS.
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
| Budgets | Accounting and admission control only — no payment processing anywhere |
