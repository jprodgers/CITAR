# Configuration

CITAR is configured in three places, and which one you need depends on what you are changing.

| Where | What | Changes take effect |
|---|---|---|
| **Environment variables** | What the process needs to start: mode, port, keys, database, e-mail | On restart |
| **The web client** | Servers, models, suites, scenarios, and on a server, policy | Immediately |
| **Files** | The server registry, saved games, benchmark runs | Immediately, or on restart for the registry |

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
| Server registry | `<checkout>/config` | `<user data>/config` |
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
| `CITAR_CONFIG_DIR` | The server registry |
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

### API keys for models

Not environment variables by default — they go to the OS credential store through the Servers page.
The alternatives, chosen per server:

| Backend | How |
|---|---|
| `keyring` | The OS credential store. The default |
| `env` | You name a variable; CITAR reads it. For containers and CI |
| `file` | scrypt + Fernet, outside the project. `CITAR_KEYS_FILE` sets the path, `CITAR_KEYS_PASSPHRASE` unlocks it without a prompt |

### Workers

| Variable | Default | Meaning |
|---|---|---|
| `CITAR_WORKER_PING_SECONDS` | `20` | Keep-alive on the worker WebSocket |

On the worker side: `CITAR_WORKER_SERVER`, `CITAR_WORKER_TOKEN`, `CITAR_WORKER_BASE_URL`,
`CITAR_WORKER_PROVIDER`, `CITAR_WORKER_MAX_CONCURRENT`, `CITAR_WORKER_NAME`. Prefer the config file
that `citar setup --worker` writes: a token on a command line ends up in the shell history and the
process list.

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
