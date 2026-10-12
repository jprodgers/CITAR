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
| See a machine and seat it | Its owner and administrators; anyone else through a grant on a server group it is in. No listing names a machine to anyone else (the lobby's availability, the machines in their quiet hours): it is 404 to them |
| Change its configuration (name, models, hardware, power and costs, quiet hours), remove it | Its owner and administrators; another member gets 404 |
| Store or delete its API key | Its owner and administrators, on a machine the server dials; a helper machine has no key on the server (404) |
| Add a machine the server dials itself, or switch one to that | Administrators, unless its provider is the dry run (it dials nothing): a member gets 403 with the reason |
| Price a machine with an electricity plan | Administrators: the plans are the household's bill. A member gets 403, and on a machine an administrator gave a plan, sees no price per kWh |
| Choose a new machine's id | Administrators; a member's new machine gets an id from the server |
| Detect its models, see what is loaded, load and unload models, read a loaded configuration, estimate memory | The owner or an administrator, on a machine the server dials or the host; for a member only the dry run. A helper machine reports its models through its helper: these answer 404 for it, and nothing is dialled or started |
| Collect the host's hardware, list LM Studio's devices | Administrators, on the host |
| Point a key at one of the server's environment variables (the `env` backend) | Administrators |
| The host-wide settings (currency, which machine is the host), the encrypted key file, electricity plans | Administrators; a member gets 404 |
| "Detect models" from an address (the lobby's) | Administrators, the dry run excepted |

A member's machine is a helper machine: the server never dials it, never drives an LM Studio for it and never reads
a key for it. The site's own machines (the host, the APIs an administrator added, the imported registry's) are
administrators' until one of them shares them in a server group, like any other machine; the administrator named as
their owner keeps nothing of them once made a member. A machine body of the wrong shape (a list where an object
belongs, say) is refused with 400 on every route that saves one. Removing a machine removes the key its row stores,
in the OS credential store or the key file, as well.

## The Servers page

**Servers** in the navigation (`#/pool`) is one page for every machine, the ones the server dials and the helper
machines side by side, with the server groups that share them below. A member sees their own machines and the ones
a group shares with them; an administrator sees every machine: the site's (the host and its online APIs), their own
and the members'. Each machine is a card, and the card of a machine you manage carries its editors, each a dialog:

| On a machine's card | What it does | Shown to |
|---|---|---|
| Test | Sends the model a one-line request through the helper | Anyone the machine is listed to, on a helper machine |
| Test connection | Asks a machine the server dials for its models | Administrators; a member on their own dry run |
| Connection & models | Name, kind, provider, slots (requests at once), the model catalogue and LM Studio's load profiles; Estimate, Load now, Copy loaded settings and Unload now on a machine the server drives | Its owner and administrators (the actions on loading: administrators) |
| Hardware & power | The hardware (imported from a collector, or typed in; Collect from this PC on the host), the power figures | Its owner and administrators (Collect: administrators, on the host) |
| Costs | Components and cost periods (an electricity plan is an administrator's to name) | Its owner and administrators |
| Quiet hours | When nothing runs on it, for anyone | Its owner and administrators |
| API key | Where the server keeps the key of a machine it dials (an environment variable: administrators) | Its owner and administrators, on a machine the server dials |
| Helper | Its worker tokens (issue, revoke) and how to run the helper | Its owner and administrators, on a helper machine |
| Sharing | The server group that shares it | Its owner and administrators |
| Disable, Enable, Remove | Disabling and removing drop its helper at once (below) | Its owner and administrators |

The page's head has **Add a machine** (a helper machine or the dry run for anyone who may register machines; a
machine the server connects to itself for administrators), **New group**, and for administrators **Electricity
plans** and **Settings** (the currency, which machine is the host). A control is shown only when its route would take
it from you: `citar/web/js/machines.js` holds the page's copy of the routes' gates, and `tests/test_servers_page.py`
holds it to the server's own, so a member's page never calls a route that would refuse them. An address of the page
before 0.1.6 still works: `#/servers` opens it and `#/servers/<id>` opens that machine (`#/pool/<id>`); a group's
address is `#/pool/groups/<id>`, which highlights it for whoever it is shared with. A group shares its machines
through its grants alone (a grant to everyone shares them with every member): a group whose visibility is `link`
admits nobody by its key yet, so there is no link to hand out.

## Revocation

Taking a machine away from its helper takes effect at once: **revoking the token** it connected with,
**disabling** the machine, **deleting** it, or **suspending its owner's account** closes the helper's connection
within a second of the change being saved. Whatever it was generating fails (it is not retried), its next request
is refused, the helper is told why and stops instead of reconnecting, and a helper that tries again with what was
taken away is refused at its hello, or, if the change was saved while its hello was being checked, as soon as it
is connected. Revoking one token leaves a helper connected with another token alone. A change made from the command
line or another process is noticed at the next keep-alive (`CITAR_WORKER_PING_SECONDS`).

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

### When a machine is busy

Everything that asks a model something goes through the server's **prompt scheduler**, which keeps one queue per
machine (the design is `design/phase3-features.md` P3.3). A machine has as many slots as its **LLM slots** setting
(`max_concurrent`), capped by the helper's own `--max-concurrent`; the scheduler sends a machine no more requests than
it has slots, so a busy machine **queues** work instead of refusing it, and nothing sleeps and retries:

- A game's model seat holds a **lease** on its machine from the moment the game is created, loaded, brought back after
  a restart or launched from a scenario (paused or not, whoever moves first), until the seat is changed or eliminated
  or the game ends or is closed; a game paused to make way for higher-priority work gives its leases up until it
  resumes (a person who chats with its model seat meanwhile is answered on a lease for that answer alone, given up
  when the answer ends), while any other pause (a person's, quiet hours, a lost connection) keeps them. Its requests
  are served in the order they come, before any pooled prompt. While the seat waits on something else (another side's
  answer in a chat, a paused game, its next turn) a lobby game's lease lends its slot. A seat's lease is held to the
  concurrency of the grant its game was admitted for: a benchmark game's to the owner's grant for benchmarks.
- **What the machine is busy with** is its leases: queued benchmark jobs wait for them, and the Queue page lists
  them. A probe's turn case holds a lease on its model's machine for the case (lending nothing), so it waits, as a
  benchmark job does, until the leases there leave a slot free (a lobby game's seat on a one-slot machine holds it up);
  its offer and message cases do not wait for a lobby game. A probe run waits for a benchmark holding its machine
  before each case, saying so (and to start, for work ranked ahead of it there). What a queued job or run says
  it waits for names only the work its reader may see; anyone else's game or run is "someone's game" or "someone's
  benchmark run", and a game paused to make way says only what kind of work it makes way for.
- A **pooled prompt** is one short job of a few model calls: a report's written analysis, each answer of a probe's
  offer or message case, the Servers page's test (and, later, a hybrid seat's diplomacy and chat answers). It holds no
  lease, so it never makes a machine look busy. It takes a free slot that no seat in the middle of its turn needs, only
  on a model the helper has loaded (unless the machine allows swaps), and never inside the machine's quiet hours.
  Waiting prompts go by priority (their class's, plus their work's queue priority, which its owner sets on the Queue
  page), then take turns between members (the one served longest ago first), then first come first served.
- **Lending.** A lobby game's seat lends its slot between its turns (and while it waits on a chat or a pause), so a
  one-slot home GPU serves pooled prompts beside a game. A prompt that borrows an idle seat's slot runs under its
  class's cap (a report's analysis five minutes and 4,096 tokens a step, a probe's answer and a chat two minutes and
  1,024, a test 30 seconds and 256), and gives the slot back after its current step as soon as the seat's turn comes:
  the seat waits for at most that one capped step. A probe's answer cut off that way (or by a benchmark) is asked
  again once the slot is free, up to three times, and is never scored as the model's own answer: one that never ran
  whole is recorded as an error, not as a failure of the model. A benchmark game on the machine holds it: pooled
  prompts are refused or, for a report's analysis (which waits for benchmarks), wait until it is gone, and a probe run
  whose answer is refused waits for the benchmark as a run, saying so; the benchmark's first request waits up to two
  minutes for those already running to finish (`drain_s` in the job's result; a benchmark that had to go ahead anyway
  is marked `contaminated`, and its timing is left out of the model's speed score).
- **Who may use a machine for pooled prompts** is a grant's **pooled prompts** purpose (`pooled`), beside **ordinary
  games** (`game`: a model seat that plays every turn) and the rest. Each prompt is admitted when it is made: the
  account needs `partial_llm` (and the work's own permission, `run_reports` or `run_probes`) and a grant for `pooled`,
  inside its window and budget; a grant's **concurrency** is how many of that account's requests may run on the
  machine at once, seats' and prompts' together. A busy machine is never a refusal: the work waits its turn. A prompt
  refused says why (no grant for pooled prompts, outside the window, the helper offline...). A report whose analysis
  waits for an offline helper, the machine's quiet hours or a grant's window waits up to half an hour to start (and
  rides out a helper that drops while it writes for as long), then the report is built without it, saying why.
  A grant limited to **ordinary games** does not cover pooled prompts: a member who should be able to test the
  machine, have reports written on it or run a probe's offers and messages there needs **pooled prompts** ticked
  too (a grant with nothing ticked covers every kind of work).
- **A grant's budget is kept per request.** Each request of work admitted under a grant with a budget reserves its
  estimate when it is given its slot and is counted at what it cost when it ends; a hard budget that a request would
  pass refuses it (a lobby game pauses until the budget's period turns, a pooled prompt is refused and its caller
  falls back), a soft one only warns ([Accounts](Design-accounts-and-sharing#budgets)). Every request a machine serves is in the
  compute ledger, given by the machine's owner and used by whoever's work it was
  ([Accounts](Design-accounts-and-sharing#the-compute-ledger-and-the-ratio)).
- **Cancelling drains on today's helper.** When the server gives up a request (a game is closed or paused, a seat is
  changed, a prompt is cancelled or runs out of time), it sends the helper `CANCEL`. The helper of this version ignores
  it and finishes the completion, so the scheduler keeps that slot taken (*draining*) until the helper's late answer
  arrives, or the request's own timeout plus 15 seconds has passed. The helper answers only over the connection the
  request went out on, so when that connection goes (the helper disconnects, or reconnects as a new connection) the
  slot is given back at once. The helper is therefore never sent more requests than it has slots, and never has to
  answer `busy`. A `busy` refusal anyway (a helper restarted with fewer slots, or one still finishing a request from
  before its reconnect) is absorbed: the scheduler holds the machine at what it has in flight until a slot frees (at
  most five seconds when nothing else is in flight), and queues the request again.
- **The game screen shows the wait.** A model seat whose request waits for its machine shows "waiting for <machine>:
  2nd in line, about 40 s" (its status `queued`) until the request runs.
- **The Servers page's test** is a pooled prompt like any other (its grant must cover pooled prompts). Each person has
  one test of a machine waiting at a time and two in all (another is refused at once), and a few dozen in ten
  minutes; it is not metered.
- **The Queue page** lists the machines you may use (your own, those your grants reach; every machine for an
  administrator), each with its slots, the seats that play every turn there (mid-turn, lending their slot between
  turns, or holding it), its jobs, and its pooled prompts, running and waiting, with each one's place in the line, how
  long it has waited, about when it starts and how long it may still wait. Work you may not see is "someone's report",
  "someone's game" and so on, with no name and no member; other members' machines are only counted ("2 other machines
  are busy"). Priority buttons are on your own work only, and **Cancel** on a prompt is for its work's owner, the
  machine's owner and administrators.
- **Generation time is what the server saw.** A step is credited the smaller of the helper's own `duration` and the
  time from sending the request to receiving its answer; time spent queueing is recorded apart and never counted as the
  model's (nor against its turn's time limit, nor in its speed score). A helper that claims more than the server
  observed (by over 10% and a second) three times within an hour gets `overreports` in its machine's flags, with an
  audit row. Token counts are capped at the request's `max_tokens` and the model's context.

An administrator's `GET /api/status` lists what the scheduler holds for each machine.

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
