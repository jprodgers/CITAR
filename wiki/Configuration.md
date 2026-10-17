# Configuration

CITAR is configured in three places, and which one you need depends on what you are changing.

| Where | What | Changes take effect |
|---|---|---|
| **Environment variables** | What the process needs to start: mode, port, keys, database, e-mail | On restart |
| **The web client** | Machines, models, suites, scenarios, and on a server, policy | Immediately |
| **Files** | Saved games, benchmark runs | Immediately |

Running CITAR on your own machine needs none of it. With no environment at all, it starts in local
mode, generates its own key, and signs you in.

---

## Where files live

```bash
citar where
```

| | A git checkout | An installed copy |
|---|---|---|
| Saved games | `<checkout>/saves` | `<user data>/saves` |
| `config/` (a 0.1.5 worker's `worker.json`, which the helper reads with `--config`; once, the old registry) | `<checkout>/config` | `<user data>/config` |
| Benchmarks | `<checkout>/benchmarks` | `<user data>/benchmarks` |
| Database, secret key | `<user data>` | `<user data>` |

`<user data>` is `%LOCALAPPDATA%\CITAR` on Windows, `~/Library/Application Support/CITAR` on macOS,
and `$XDG_DATA_HOME/citar` (usually `~/.local/share/citar`) on Linux.

A checkout keeps its state beside the code because that is the development workflow — your test
games are where you can see and delete them. An installed copy must not write into
`site-packages`, which is often not writable and is the wrong place for a save file.

**The database and the secret key are always in the user directory, never in a checkout.** A
project folder may be synced to OneDrive or Dropbox, and a sync client copying a SQLite file
mid-write corrupts it. Password hashes do not belong in a synced folder either.

### Overriding

| Variable | Sets |
|---|---|
| `CITAR_STATE_DIR` | Everything below, in one go |
| `CITAR_SAVE_DIR` | Saved games, and maps, scenarios, probes, lab, reports and the usage ledger with them |
| `CITAR_CONFIG_DIR` | The `config/` folder: a 0.1.5 worker's `worker.json`, and a pre-0.1.6 `servers.json` until it is imported |
| `CITAR_BENCH_DIR` | Benchmark suites and runs |
| `CITAR_DATA_DIR` | Database and secret key |

---

## Environment variables

Only needed to run a server. [env.example](https://github.com/jprodgers/CITAR/blob/main/env.example) is the annotated copy; this is the
reference.

### Mode and identity

| Variable | Default | Meaning |
|---|---|---|
| `CITAR_MODE` | `local` | `local` — loopback, one auto-signed-in owner, no TLS. `server` — real sessions, TLS required, refuses to start half-configured |
| `CITAR_PUBLIC_ORIGIN` | | The URL browsers use, e.g. `https://citar.example.com`. Required in server mode, must be `https://`. Sign-in links and OAuth callbacks are built from it |
| `CITAR_SECRET_KEY` | generated locally | Signs sessions and one-shot tokens. Generate once and keep it: changing it signs everybody out. `python -c "import secrets; print(secrets.token_urlsafe(48))"` |
| `CITAR_HOST` | `127.0.0.1` | Bind address. Keep it on loopback behind a proxy |
| `CITAR_PORT` | `8765` | |
| `CITAR_DEBUG` | off | Enables the debug endpoints. Never on a public server |

**`CITAR_PUBLIC_ORIGIN` must match exactly what browsers use**, including whether there is a `www`.
`localhost` and `127.0.0.1` are different origins, and a mismatch refuses every write with an error
that looks like a bug in the client.

### Behind a proxy

| Variable | Default | Meaning |
|---|---|---|
| `CITAR_BEHIND_PROXY` | `0` | Read the client IP from `X-Forwarded-For` |
| `CITAR_TRUSTED_PROXY_HOPS` | `1` | How many proxies add an entry |

Getting the hop count wrong breaks rate limiting in one of two ways: too high and a client can
forge its own address past the limiter, too low and every request appears to come from the proxy.
One nginx or Caddy in front is `1`.

### Database

| Variable | Default | Meaning |
|---|---|---|
| `CITAR_DB_URL` | SQLite in the data directory | e.g. `postgresql+psycopg://citar:PASSWORD@127.0.0.1:5432/citar` |

The schema is written to the intersection of SQLite and PostgreSQL, so either works. SQLite is
fine at this scale: the game engine is the bottleneck long before the database is.

### Accounts

| Variable | Default | Meaning |
|---|---|---|
| `CITAR_REGISTRATION` | `invite` | `open`, `invite` or `closed`. The boot default; administrators change it live |
| `CITAR_SESSION_DAYS` | `30` | Absolute session lifetime |
| `CITAR_SESSION_IDLE_HOURS` | `336` | Signed out after this long with no request |

### E-mail

| Variable | Meaning |
|---|---|
| `CITAR_SMTP_HOST`, `CITAR_SMTP_PORT` | The relay. 587 with `starttls`, 465 with `ssl` |
| `CITAR_SMTP_SECURITY` | `starttls`, `ssl` or `none` |
| `CITAR_SMTP_USER`, `CITAR_SMTP_PASSWORD` | |
| `CITAR_MAIL_FROM`, `CITAR_MAIL_FROM_NAME` | Must be an address the account may send as |

Without SMTP there is no sign-up verification and no password reset. Invitations still work, which
is why invite-only is the default: a server can go live before mail exists.

### Single sign-on

`CITAR_OAUTH_<PROVIDER>_CLIENT_ID` and `_CLIENT_SECRET` for `GOOGLE`, `GITHUB`, `DISCORD` and
`MICROSOFT`, plus `CITAR_OAUTH_MICROSOFT_TENANT` (`common` accepts any Microsoft account).

A provider with no credentials simply does not appear on the sign-in page. Callback URL:

```
https://<your domain>/api/auth/oauth/<provider>/callback
```

[server/OAUTH.md](Single-sign-on) has the registration steps for each provider.

### Anti-bot

`CITAR_HCAPTCHA_SITE_KEY` and `CITAR_HCAPTCHA_SECRET`. Only needed with open registration.

### The machines: in the database, not a file

Since 0.1.6 every machine that can run a model is a row of the database's `servers` table: the computer CITAR
runs on (the **host**), the online APIs and LAN machines the server dials itself, and the members' machines behind
the CITAR helper ([Workers](Workers)). The electricity plans and the currency are kept there too. They are
edited on the Servers page; there is no registry file to edit any more.

**`config/servers.json`** was the registry before 0.1.6. It is imported **once**: at the first start that has an
administrator, or else the moment the first administrator is made (`admin_cli create-admin`, the setup wizard, an
invitation with the admin role; in local mode the owner, at the first start). Every machine keeps its id, so seats,
suites, saved games, the usage ledger and stored API keys keep resolving; each becomes the first administrator's,
in no server group (share one from the Servers page when members should use it), with its `max_parallel` as its
slots. The file is then **renamed** `servers.json.imported-<date>-<time>` beside itself, never deleted, and never
read again; a file that reappears later is left alone, with a warning at each start. A `servers.json` in another
`config/` folder (a checkout and an installed copy can share one database) is **not** imported once one was: the
warning says so, and its machines are added on the Servers page by hand if they are wanted. A fresh server-mode
database with no administrator imports nothing and says so in its log. With no file at all, the first run makes the
default machines the file used to start with (this computer as the host, the Anthropic API, the dry run), once the
first administrator is saved. A database where the old Servers page's "import" was used keeps the copies it made
under new ids, beside the machines imported under their own; the log and the audit row name them for removal.

The host and the site's online APIs are the site's: an administrator manages them, whoever the import named as
their owner. The power sampler follows whichever machine is the host, so a host that appears with the first
administrator is sampled without a restart. A helper machine samples itself: its CITAR helper sends a minute of
10-second readings (the GPUs' draw, the CPU's utilisation) at a time, which the server writes as the machine's power
rows (`saves/usage/power-<machine>-YYYY-MM.jsonl`, the host sampler's format), so costing prices its energy from what
it drew. Where a GPU's draw does not read (an AMD or Intel GPU, a Mac), the rows carry the CPU alone, and costing
estimates the GPU's share from the machine's power figures while its model works, as for a machine without samples
(such a minute counts as estimated, and the machine's metered total leaves those rows out). A machine whose **Live
sampling** is `off` (its power settings' `sampling`) gets no rows, from the host's sampler or its helper; the helper's
own `--no-power` sends none.
`citar setup --local` writes its machines into the database when there is one, else into `servers.json` for the
first start to import.

A machine's **settings** live on its row too, edited in the Servers page's **Machine settings…** dialog
([Workers](Workers#machine-settings-and-what-a-helpers-report-changes)): its LLM slots (`max_concurrent`,
the site's one number for it), its capabilities (`capabilities`: whether it takes model work, whether pooled prompts
may make it swap models, and its CPU sharing, all sent to its connected helper, which acknowledges what it will do),
and what its helper's reports may change
(`hardware_mode`, `models_mode`, `power_mode`, each `auto` or `manual`; by default the hardware and the models
follow the helper and the power figures are yours). Its helper's last report is kept on the row
(`hardware_detected`, `detected_at`) whatever those say. Its `trust_tier` and pinned hardware classes are an
administrator's to set there.

**The host's CPU** plays the lab's games (the server's own job host, [Bots: the lab](Scripted-bots#the-lab)): the host
machine's CPU sharing is what that job host runs under, and it is on by default, unlike any other machine's. Its
defaults: in local mode half the logical CPUs as threads (one game each) and that share of the machine, with a
quarter of the memory up to 512 MB a thread (at least 256 MB); in server mode one thread, half the machine and
min(1,024 MB, a quarter of the memory), about 240 MB on a one-vCPU box of 961 MB, below what the OOM killer would
take from the server itself. A figure left at 0 there is the default's (the job host is always given concrete
limits), and the kinds of work it takes default to all three (`lab_game`, `ranking_game`, `reference_game`). Turn
CPU sharing off in its dialog to stop the server playing the lab at all; change the figures and the job host
restarts with them once the games it is playing finish. The limits hold hard where the operating system allows (a
Job Object on Windows; a `systemd --user` scope on Linux where the user's manager makes one, else `RLIMIT_DATA` and
`nice`, the CPU soft; on macOS both soft), as the Lab page shows.

### API keys for models

Not environment variables by default — they go to the OS credential store through the Servers page.
The alternatives, chosen per machine:

| Backend | How |
|---|---|
| `keyring` | The OS credential store. The default |
| `env` | You name a variable; CITAR reads it. For containers and CI. An administrator's to choose: it names one of the server's own variables |
| `file` | scrypt + Fernet, outside the project. `CITAR_KEYS_FILE` sets the path, `CITAR_KEYS_PASSPHRASE` unlocks it without a prompt |

Keys are stored under the machine's id, which the import keeps.

### Credits and hardware classes

The compute ledger counts what each member's machines **gave** other members and what their work **used** of others'
machines and the site's ([Accounts](Design-accounts-and-sharing#the-compute-ledger-and-the-ratio)), in **credits**. An event's
credits are priced when it is written, from the machine's **hardware class** of its kind:

| Kind | Credits | Default class and rate |
|---|---|---|
| `gpu` | a model step's generation seconds × the rate | GPU (unclassified): 1.0 |
| `cpu` | the host's CPU core-seconds × the rate | CPU core (unclassified): 0.05 |
| `api` | a step's token cost at the model's prices, in the registry's currency, × the rate | Online API: 1,000 |

The compute rates are an energy proxy, one credit for roughly 300 W for a second (an unclassified GPU's
generation-second; a CPU core is about 15 W); an online API's 1,000 credits per unit of currency is roughly the
generation time a home GPU spends producing what that much buys from an API, so a member who uses the site's paid key is
counted in the same unit as one who uses a member's GPU. An API step whose model has no price is credited 0 and flagged
`unpriced`.

A machine's class of a kind is the first class of that kind, by priority (highest first), whose **match** fits the
machine's hardware, else the kind's default:

| Match key | Fits when |
|---|---|
| `gpu_name` | a regular expression matches one of its GPUs' names (case ignored) |
| `vram_gb_min` | its largest GPU has at least that much memory |
| `cpu_name` | a regular expression matches its CPU's model |
| `cores_min` | its CPU has at least that many cores (else threads) |

The class follows the hardware: whatever changes a machine's hardware details (its helper's scan with the machine's
hardware mode `auto`, the hardware dialog, an Apply of the last scan, Collect on the host) classifies it again, unless
an administrator pinned its classes (`class_override`: Machine settings…, Class and trust, or
`PUT /api/admin/machines/{id}/class`; [API](HTTP-and-tool-API#a-machines-settings)). Each kind keeps one default class, which
cannot be removed.

Administrators edit the classes over HTTP (`GET`/`POST /api/admin/hardware-classes`,
`PUT`/`DELETE /api/admin/hardware-classes/{id}`; [API](HTTP-and-tool-API#the-compute-ledger)). A new rate applies to the events
written after it; to recompute a period at today's classes, rates and token prices:

```bash
citar admin reprice --since 2026-10-01 [--until 2026-10-08]     # UTC dates; the hours' rollups follow
```

An item an administrator marks as the server's own data collection counts as given by whoever's machine served it and
used by nobody:

```bash
citar admin set-collection benchmark_run <run id> on
```

Budgets on grants are counted in the same credits ([Accounts](Design-accounts-and-sharing#budgets)).

### Workers

| Variable | Default | Meaning |
|---|---|---|
| `CITAR_WORKER_PING_SECONDS` | `20` | Keep-alive on a CITAR helper's WebSocket (`welcome.ping_s`, 5 to 300): a helper silent for 70 s is dropped |
| `CITAR_HELPER_MANIFEST_URL` | GitHub's release download of this server's version in server mode (`https://github.com/jprodgers/CITAR/releases/download/v{version}/citar-helper-manifest.json`); none in local mode | Where the server fetches the signed manifest of the CITAR helper it relays to its helpers (`{version}` its own version; the signature at the same address with `.sig`), at most hourly and never on a request. `https` only (`http` to this machine for tests); `off` relays none |
| `CITAR_HELPER_MANIFEST_FILE` | none | A manifest on disk instead (its signature beside it as `<file>.sig`): an offline server's, or a test's. It wins over the URL |

Only a manifest that verifies against the update keys built into the server's extension (the helper's own,
`crates/citar-jobs/update-keys.pub`, none until the owner's first signed release) is relayed; a failed fetch keeps the
last good one. Every helper's welcome carries it, a new one goes at once to the connected helpers older than it, and
each helper decides by its own `updates` setting ([Workers](Workers#updates)). The Servers page shows each
helper machine's version beside the latest signed helper, and where its helper is with an update.

The limit of refused worker tokens per address (`worker_auth`: 20 different tokens in five minutes) is counted before
any token is looked up, from the address `CITAR_BEHIND_PROXY` and `CITAR_TRUSTED_PROXY_HOPS` give, as every other
limit, one hello of an address at a time (so hellos sent together cannot all get past it); it holds in local mode
too.

On the helper's side (the CITAR helper, `citar-helper`): `CITAR_WORKER_SERVER`, `CITAR_WORKER_TOKEN`,
`CITAR_WORKER_BASE_URL`, `CITAR_WORKER_PROVIDER`, `CITAR_WORKER_MAX_CONCURRENT`, `CITAR_WORKER_NAME`,
`CITAR_WORKER_API_KEY_ENV` and `CITAR_WORKER_HARDWARE_FILE` (the hardware to report instead of a scan: a collector's
file, as `--hardware`), each below the flag and the settings file ([Workers](Workers#the-helpers-settings-citar-016)).
Its update policy is `updates` (`auto`, `notify` or `off`; `--updates`) in `helper.json` ([Updates](Workers#updates)).
Prefer the settings file `citar-helper setup` writes (`helper.json`): a token on a command line ends up in the shell
history and the process list.

---

## Runtime policy

Settings an administrator changes while the server runs, from **Server** in the top navigation or
with `citar admin policy`:

| Setting | Default | Meaning |
|---|---|---|
| `registration` | from the environment | `open`, `invite`, `closed` |
| `probation_enabled` | `true` | New accounts can play and watch but cannot register hardware, invite or publish |
| `probation_skips_sso` | `true` | Sign-ins from a provider that asserts a verified address skip probation |
| `default_invite_quota` | `0` | Invitations each member may send |
| `default_max_concurrent_games` | `2` | Games one account may run at once. The lever that keeps a small machine responsive |
| `aggregate_min_contributors` | `3` | Contributors before a pooled average is shown, so a public number cannot be read back as one account's data |
| `welcome_message`, `closed_message` | | Shown on the sign-in page |

Every change is written to the audit log.

---

## Checking it

```bash
citar doctor
```

Reports the effective configuration, what is missing, and what each gap costs — without printing
any secret.


---

*This page is generated from [`docs/CONFIGURATION.md`](https://github.com/jprodgers/CITAR/blob/main/docs/CONFIGURATION.md) and any edit made here will be overwritten. Corrections are welcome as a pull request.*
