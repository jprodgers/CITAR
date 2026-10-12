# Workers: lending a GPU to a server

The machine with the GPU is usually the machine you cannot expose. It is behind a home router, on a
changing address, and opening a port to the internet so a server can reach it is both fiddly and a
bad idea.

So the connection runs the other way. The worker dials **out** to the CITAR server over a
WebSocket, authenticates with a token, and serves model requests down that same connection. Nothing
needs to be opened at the house, and the server never needs to know where the worker is.

```
   CITAR server                          your PC
  (runs the games)                  (runs the models)
        |                                   |
        |  <=== outbound wss:// ==========  |
        |                                   |
        |  "complete this"  ------------>   |  LM Studio / Ollama
        |  <-----------  "here you go"      |       |
                                            |      GPU
```

**The server runs the games; workers only serve models.** The game engine, the bots and the state
all stay on the server. A worker is a model endpoint with a tunnel around it.

---

## For the person running the server

Register the machine and issue a token:

```bash
citar admin add-server --name "Alice's desktop"
citar admin worker-token <server id>     # another token for the same machine
citar admin workers                      # list them
```

Send the token to its owner through something private. It is a credential: anyone holding it can
present themselves as that machine.

Once the worker connects it appears on the **Servers** page like any other machine, and its models
can be used in seats and benchmark suites. Availability windows and budgets apply to it the same
way.

---

## Who may do what to a machine

Every machine is a row of the server's database: the host, the online APIs and LAN machines the server dials
itself (**direct**), and the machines behind a CITAR helper (**helper machines**). Who may touch one depends on what
the action touches:

| Action | Who |
|---|---|
| See a machine and seat it | Its owner and administrators; anyone else through a grant on a server group it is in |
| Change its configuration (name, models, hardware, power and costs, quiet hours, its key), remove it | Its owner and administrators; another member gets 404 |
| Add a machine the server dials itself, or switch one to that | Administrators, unless its provider is the dry run (it dials nothing): a member gets 403 with the reason |
| Detect its models, see what is loaded, load and unload models, read a loaded configuration, estimate memory | The owner or an administrator, on a machine the server dials or the host; for a member only the dry run. A helper machine reports its models through its helper: these answer 404 for it, and nothing is dialled or started |
| Collect the host's hardware, list LM Studio's devices | Administrators, on the host |
| Point a key at one of the server's environment variables (the `env` backend) | Administrators |
| The host-wide settings (currency, which machine is the host), the encrypted key file, electricity plans | Administrators; a member gets 404 |
| "Detect models" from an address (the lobby's) | Administrators, the dry run excepted |

A member's machine is a helper machine: the server never dials it, never drives an LM Studio for it and never reads
a key for it. The site's own machines (the host, the APIs an administrator added, the imported registry's) are
administrators' until one of them shares them in a server group, like any other machine.

## Revocation

Taking a machine away from its helper takes effect at once: **revoking the token** it connected with,
**disabling** the machine, **deleting** it, or **suspending its owner's account** closes the helper's connection
within a second of the change being saved. Whatever it was generating fails (it is not retried), its next request
is refused, the helper is told why and stops instead of reconnecting, and a helper that tries again with what was
taken away is refused at its hello. Revoking one token leaves a helper connected with another token alone. A change
made from the command line or another process is noticed at the next keep-alive (`CITAR_WORKER_PING_SECONDS`).

---

## For the person lending the machine

The simplest way is the **CITAR helper**: the worker as one file, with nothing else to install.
The **Servers** and **Models** pages offer the right download for your computer (Windows, macOS on
Apple silicon, Linux x64 and ARM64); the files are also on every
[GitHub release](https://github.com/jprodgers/CITAR/releases/latest). Start it with no arguments —
double-click it on Windows — and it asks for the server's address and the token once, remembers
them in `helper.json` in your CITAR folder, and connects straight away every time after that. It
takes the same options as `citar worker` below.

On macOS and Linux, make the download executable first (`chmod +x citar-helper-*`; on macOS also
`xattr -d com.apple.quarantine citar-helper-macos-arm64`, since the build is not notarised).

If the server restarts, the helper reconnects by itself (after 2 s, then backing off to a minute).
Only a refused token stops it for good.

Or install CITAR itself on that machine:

```bash
curl -fsSL https://raw.githubusercontent.com/jprodgers/CITAR/main/install.sh | bash -s -- --worker
```

or, if CITAR is already installed:

```bash
citar setup --worker
```

It asks for the server URL and the token, finds the model server running on your machine, checks
both, and writes `worker.json` with mode 0600 — the token is never passed on a command line, where
it would land in your shell history and in the process list.

Then:

```bash
citar-worker --config ~/.local/share/citar/config/worker.json
```

On Linux, the wizard can install a systemd unit so it starts with the machine. `Restart=always`,
because a dropped home connection should reconnect rather than stay down.

### Quiet hours

If the GPU is in a room somebody sleeps in:

```json
{"quiet_hours": ["21:00-06:00"]}
```

or per weekday, `"Sat 23:00-08:00"`. During a window the worker takes no new work. Benchmarks and
lab runs pause; a game somebody is playing warns rather than stopping.

### What the server can and cannot do

It can ask your machine to run a completion with one of the models you exposed. That is the whole
protocol.

It cannot read your files, run commands, see other things on your network, or use models you did
not list. The worker only speaks to the base URL you configured, and only forwards completion
requests to it.

Stop lending at any time: stop the worker, or revoke its token (or remove the machine) on the
Servers page; the server drops the connection at once. Neither needs the other's cooperation.

---

## Troubleshooting

**Connects, then disconnects.** The token is wrong or has been revoked, the machine is disabled
or removed, or its owner's account is suspended. The helper's log says which ("The server rejected
this worker", or the server's reason). Issue a new token on the Servers page.

**`wss://` fails immediately.** The server's reverse proxy is not passing the WebSocket upgrade.
The nginx site and Caddyfile in `deploy/` do; a hand-written one often does not.

**Connects, but never gets work.** The worker offers models; the *server* decides which seat uses
which. Check that a seat or suite is configured to use that server's model. Also check the worker's
own endpoint is reachable from the worker machine — `citar doctor` on that machine says.

**Everything looks fine and completions fail.** The model is probably not loaded. A catalogue entry
is not the same as a model in memory.

---

## A note for operators

A worker is somebody else's computer doing work for your server. Two things follow:

- **Their availability is not yours.** Windows, quiet hours and a laptop being closed are all
  normal. Benchmarks that depend on a worker should expect to pause.
- **Their costs are theirs.** The registry records the machine's hardware and electricity so
  reports attribute the cost to the right place. Non-owners never see a server's cost
  configuration — the tariff, hardware prices and key backend are visible only to its owner.


---

*This page is generated from [`docs/server/WORKERS.md`](https://github.com/jprodgers/CITAR/blob/main/docs/server/WORKERS.md) and any edit made here will be overwritten. Corrections are welcome as a pull request.*
