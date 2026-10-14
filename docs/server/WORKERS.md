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
| Its machine settings: LLM slots, model swaps, CPU sharing, what its helper's reports may change, applying the last one | Its owner and administrators; a member a group lends it to gets 403, any other member 404 |
| Set its trust tier, pin the hardware classes it is priced by | Administrators; anyone else gets 404 |
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
| Machine settings… | Capabilities (LLM on or off, slots, model swaps for pooled prompts, CPU sharing), what its helper's reports may change (hardware, models and power, each auto or manual) with the last report and Apply, its hardware class and trust tier ([below](#machine-settings-and-what-a-helpers-report-changes)) | Its owner and administrators (pinning the class and the trust tier: administrators) |
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

## Machine settings, and what a helper's report changes

**Machine settings…** on a machine's card is a dialog in three tabs (`citar/web/js/machine_settings.js`; the routes are
in [API](../API.md#a-machines-settings)).

**Capabilities.** Whether the machine takes model work at all (LLM off refuses every seat, test and pooled prompt on
it, saying why), its **LLM slots** (how many requests it takes at once, the site's one number for it; its helper's own
`--max-concurrent` still caps it), and whether pooled prompts may make it **swap models** (off by default: a report's
analysis, a probe's answer, a test or a hybrid seat's diplomacy runs only on a model it has loaded, or one a game is
using there). These take effect at once: the dialog shows what the prompt scheduler has in effect. **CPU sharing**
(threads, CPU %, memory and the kinds of CPU work: lab games, ranking games, reference games) is kept for the 0.1.6
helper, which runs CPU work; the helper of this version (worker protocol 1) is sent none of these settings. Its limits
are hard where the operating system allows, and the helper's own local settings always win.

**What the helper's report changes.** Each time it connects, the helper reports the machine's hardware (its scan, the
file given with `--hardware FILE`, or nothing with `--no-hardware`) and the models its model server lists, with which
are loaded. The server **keeps the last report whatever the settings**, with its time, and the dialog shows how it differs from the machine's configuration. What it does with it
is the machine's choice, per section:

| Section | `auto` | `manual` | Default |
|---|---|---|---|
| Hardware | The scan becomes the hardware details, and the machine's hardware class follows them | The details are yours: nothing changes | auto |
| Models | The catalogue is updated: a new model is added, loaded ones are marked, one no longer reported is kept, marked "not reported", until you remove it | The catalogue is yours: only which of its models are loaded is marked; nothing is added or removed | auto |
| Power | The scan's suggested figures (rough guesses from the hardware) become the power figures | The figures are yours: nothing changes | manual |

**Apply** copies the last report in, one section or all of it, whatever the modes ("Apply all of the last report"
copies each section the report has something for: a helper started with `--no-hardware` reports models alone, and a
collector's file may suggest no power figures). A figure in a scan that is not a usable number (not a number at all,
infinite, or too large for any machine) is dropped as the scan is kept, as are parts of a shape no collector writes,
whichever way the hardware details come in (the helper, a hardware file, the Hardware & power dialog): the rest of the
report is kept and applied as usual. A report never changes the machine's name, slots, quiet hours, costs, components or key, nor a model's display name (unless the helper gives the
model a name of its own), inference settings, load profiles, prices or whether it is offered. So a machine whose
hardware you typed in keeps it through every reconnect once Hardware is manual, and its last scan is there to compare
and apply.

**Class and trust.** The machine's **hardware class** prices its work in the compute ledger
([Configuration](../CONFIGURATION.md#credits-and-hardware-classes)): it follows the hardware details unless an
administrator **pins** it, here. Its **trust tier** (new, provisional, trusted, quarantined) is set by administrators;
today it is a label, which a later version sets by itself and acts on. Both are shown to the machine's owner. An
administrator also sees, here and on the card, when its helper has **over-reported** its generation time (more than the
server observed, by over 10% and a second, three times within an hour): its work is credited the time the server
observed whatever it reports.

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

### The helper's settings (CITAR 0.1.6)

CITAR 0.1.6 replaces the Python worker with a Rust helper, `citar-helper`, under the same download
names. It reads every setting the 0.1.5 helper read, with the same meaning, so a `helper.json`, a
`worker.json` or a command line written for 0.1.5 keeps working. Each setting comes from the first
place that has it: a flag, then the settings file (`--config FILE`, or else `helper.json` in the
helper's home), then an environment variable, then its default. A flag given as `0` or empty counts
as not given, and so does an empty value in the file.

| Setting | Flag | `helper.json` key | Variable | Default |
|---|---|---|---|---|
| The CITAR server | `--server URL` | `server_url` | `CITAR_WORKER_SERVER` | none: required |
| The worker token | `--token` | `token` | `CITAR_WORKER_TOKEN` | none: required |
| The model endpoint | `--base-url` | `base_url` | `CITAR_WORKER_BASE_URL` | `http://localhost:1234/v1` |
| What serves the models | `--provider` | `provider` | `CITAR_WORKER_PROVIDER` | `lmstudio` (or `ollama`, `openai_compatible`, `openai`) |
| The API key's variable | `--api-key-env` | `api_key_env` | `CITAR_WORKER_API_KEY_ENV` | none (`openai` reads `OPENAI_API_KEY`) |
| Requests at once | `--max-concurrent` | `max_concurrent` | `CITAR_WORKER_MAX_CONCURRENT` | 1 (at most 16) |
| The machine's name | `--name` | `name` | `CITAR_WORKER_NAME` | none: the server's name for it |
| Only these models | `--model` (repeat) | `models` | | every model the endpoint lists |
| Quiet hours | `--quiet` (repeat) | `quiet_hours` | | none |
| A hardware file to report | `--hardware FILE` | `hardware_file` | `CITAR_WORKER_HARDWARE_FILE` | none |
| Report no hardware | `--no-hardware` | | | |
| Skip TLS checks | `--insecure` | `insecure` | | off, and only for a server on this machine or a private network |
| The helper's home | `--home DIR` | | `CITAR_DATA_DIR` | the per-user CITAR folder |
| Also trust this CA (PEM) | `--ca-file FILE` | `ca_file` | | the system's certificates only |
| Serve no models (lend CPU only) | `--no-llm` | `no_llm` | | |
| Lend spare CPU | `--cpu` (this run) | `cpu.enabled` | | **off** |
| The CPU ceiling | | `cpu.threads`, `cpu.cpu_percent`, `cpu.memory_mb`, `cpu.work` | | half the logical CPUs, 50%, a quarter of the memory, every kind of work |
| Updates | `--updates` | `updates` | | `auto` (or `notify`, `off`) |
| Stream from the endpoint | | `stream` | | `true` |
| Send no power samples | `--no-power` | `no_power` | | |
| Run as a service | `--service` | | | |

**The first run.** Started with nothing configured, at a terminal, the helper asks for the server's
address, a token, and whether to lend this machine's spare CPU (the default is no), and offers to
remember them in `helper.json`. `citar-helper setup` asks the same at any time, to give the helper a
new token for instance, and keeps the file's other settings. A double-clicked helper that cannot
start says why and waits for Enter before its window closes.

**The home** is the per-user CITAR folder (`%LOCALAPPDATA%\CITAR` on Windows,
`~/Library/Application Support/CITAR` on macOS, `~/.local/share/citar` on Linux), unless `--home`
or `CITAR_DATA_DIR` names another. `helper.json` sits at its root, as in 0.1.5; the helper keeps its
own files under `helper/` there: `machine-key.json`, and `logs/helper.log` (rotated at 5 MB, three
old logs kept; the token never appears in it). It writes nothing outside its home.

**CPU lending is off unless you turn it on, on the machine itself.** The server's machine settings
can ask for CPU work, but the helper does none unless this machine's own switch is on: `setup`'s
question, `"cpu": {"enabled": true}` in `helper.json`, or `--cpu` for one run. A 0.1.5
`helper.json` has no `cpu` section, so a helper moved over from 0.1.5 lends no CPU until you say so,
and the server's machine dialog shows "not lent by this machine's owner". The `cpu` figures are a
ceiling: the helper takes the smaller of each of the server's figures and yours, and tells the
server what it will actually do.

**The machine key.** On its first run the helper makes a key for the machine (`helper/machine-key.json`).
The server registers it at the first connection and refuses a later connection that presents
another one, until the machine's owner resets the key on the Servers page; `citar-helper --version
--home DIR` prints its fingerprint, which the Servers page shows too, so you can check the machine
is yours. The file must be readable by you alone: the helper writes it so (mode 0600; on Windows,
access for you, SYSTEM and Administrators only) and refuses to start when anyone else can read it,
printing the command that fixes it (`chmod 600 …`, or an `icacls …` line on Windows). It warns, with
the same kind of command, about a `helper.json` others can read, since that holds the token.

**When the server says no.** A refused token or key, or a machine taken away (revoked, disabled,
deleted, suspended, or replaced by another helper with the same token), is permanent: run from a
console the helper says why and exits with code 78; as a service (`--service`) it waits an hour
before trying again, so no service manager restarts it in a loop. A server that does not speak the
helper's protocol (a 0.1.5 server, say) is tried again every 10 minutes, and the helper connects by
itself once the server is upgraded. Anything else (a restart, a dropped connection) is retried after
2 s, then backing off to a minute.

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
- **A grant's budget is kept per request.** Each request of work admitted under a grant is held to the budget the
  grant has when the request runs (one set or changed later holds the work already running); it reserves its
  estimate when it is given its slot and is counted at what it cost when it ends; a hard budget that a request would
  pass refuses it (a lobby game pauses until the budget's period turns, a pooled prompt is refused and its caller
  falls back), a soft one only warns ([Accounts](ACCOUNTS.md#budgets)). Every request a machine serves is in the
  compute ledger, given by the machine's owner and used by whoever's work it was
  ([Accounts](ACCOUNTS.md#the-compute-ledger-and-the-ratio)).
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
