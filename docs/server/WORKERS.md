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

## For the person lending the machine

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

Stop lending at any time: stop the worker, or ask the operator to revoke the token. Neither needs
the other's cooperation.

---

## Troubleshooting

**Connects, then disconnects.** The token is wrong or has been revoked. Reissuing a token
invalidates the old one.

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
