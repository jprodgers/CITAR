# Troubleshooting

Start here:

```bash
citar doctor
```

It checks versions, dependencies, directories and their permissions, configuration, the database,
the ruleset, every model endpoint in your registry, and whether the port is free. It changes
nothing, contains no secrets, and is what a bug report should include.

---

## It will not start

**`No Python 3.11+ found`** — CITAR needs 3.11 or newer. On Debian or Ubuntu, `python3-venv` is a
separate package and its absence is the most common install failure: `sudo apt install python3-venv`.

**`Address already in use`** — something is on port 8765, often another CITAR.

```bash
citar serve --port 8766
```

The Windows launcher picks a free port by itself.

**`CITAR cannot start: CITAR_PUBLIC_ORIGIN is required in server mode`** — server mode refuses to
start half-configured rather than starting and failing later in ways that are hard to trace. Set
what it names, or use local mode.

**The service starts and immediately stops (systemd)**

```bash
journalctl -u citar -n 50 --no-pager
```

Usually the environment file: a missing secret key, a database path the service account cannot
write, or a `CITAR_PUBLIC_ORIGIN` that is not `https://`.

---

## An AI player does nothing

The most common problem, and it is nearly always one of four things.

1. **Is the model server running?** `citar doctor` lists every endpoint and whether it answered.
   In LM Studio the local server is a separate switch from the app being open — Developer tab,
   "Start server".
2. **Is the right base URL configured?** LM Studio is `http://localhost:1234/v1`, Ollama
   `http://localhost:11434/v1`, llama.cpp `http://localhost:8080/v1`. A missing `/v1` fails in a
   way that looks like a CITAR bug.
3. **Is the model loaded?** Detection lists a catalogue; a model in the catalogue is not
   necessarily in memory.
4. **What did it actually say?** The seat's error is shown beside it in the lobby, and in full at
   `/api/games/<game id>/debug/errors`.

### It connects, but the turn does nothing useful

Look at **📊 AI stats** for that seat:

| What you see | Usually means |
|---|---|
| Turns ending in `no_tool_calls` | The model is replying in prose. Set **Tool mode** to `json` |
| Turns ending in `stalled` | It is repeating calls that change nothing. Often a context length too small for the briefing |
| Turns ending in `time_limit` | Too slow for the limit. Lower the reasoning effort, use a smaller model, or raise `max_turn_seconds` |
| High **malformed (repaired)** | Native tool calling is not working well for this model. `json` mode |
| High **repeats refused** | The model is not reading the results of its own actions. Usually a small model or a truncated context |

### It was working and now it is not

A server restart drops in-memory games. Reload from the autosave in the lobby — nothing is lost,
autosaves are written every turn.

---

## Games are slow

A turn is slow for one of two reasons, and they need opposite fixes.

**The model is slow.** Check seconds per model call in AI stats. Lower the reasoning effort to
`low` (about four times faster on local models, with no loss in play quality), use a smaller model,
or raise the context only as far as you need.

**The engine is slow.** Bot turns and large maps are CPU-bound and single-threaded per game. A
Gargantuan map with 24 civilizations is slow no matter what hardware you have. On a small server,
lower `default_max_concurrent_games`.

---

## Files in the wrong place

```bash
citar where
```

A checkout writes beside the code; an installed copy writes to your user directory. If that is not
what you want, set `CITAR_STATE_DIR` — see [CONFIGURATION.md](CONFIGURATION.md#where-files-live).

**Games from an older CITAR do not load.** When the ruleset version changes, old saves cannot be
loaded, because the rules they were played under no longer exist. The release notes say when this
happens.

---

## The browser shows nothing, or an old version

Hard-reload: `Ctrl+Shift+R`, or `Cmd+Shift+R`. The client is served as plain files with no build
step, so a stale cache shows old JavaScript against a new API.

Check the browser console. `Failed to load module script` after an upgrade means a cached file; the
hard reload fixes it.

---

## Sign-in problems on a server

**Every write is refused, or sign-in bounces back to the sign-in page.** `CITAR_PUBLIC_ORIGIN` does
not match the origin the browser is actually using. `localhost` and `127.0.0.1` are different
origins; so are `example.com` and `www.example.com`.

**The OAuth provider says the redirect URI does not match.** It must be exactly
`https://<your domain>/api/auth/oauth/<provider>/callback`, registered with the provider.

**Verification e-mails never arrive.** Send a test from **Server → Send test message**, or:

```bash
citar admin test-email --to you@example.com
```

The error text matters: "authentication failed" is a wrong password; "certificate verify failed" is
usually the wrong port for the security setting (587 with `starttls`, 465 with `ssl`).

Mail from a new subdomain often lands in spam until SPF and DKIM are in place.

---

## Workers

**The worker connects and disconnects.** Check the token — it is per-server, and reissuing one
invalidates the old.

**The worker connects but models never run on it.** The worker serves its models to the server; the
*server* decides which seat uses which model. Check that the seat is configured to use that
server's model, and that the worker's own endpoint is reachable *from the worker machine*.

**`wss://` connection fails immediately behind a proxy.** The proxy must pass the WebSocket
upgrade. The nginx site and the Caddyfile in `deploy/` do; a hand-written proxy config often does
not, and the symptom is a site that loads and then silently stops updating.

---

## Still stuck

Open an issue with the output of `citar doctor`, what you did, and what happened:
<https://github.com/jprodgers/CITAR/issues>

For a security problem, please report it privately:
<https://github.com/jprodgers/CITAR/security/advisories/new>
