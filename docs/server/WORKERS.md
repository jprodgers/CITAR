# Workers: lending a GPU to a server

The machine with the GPU is usually the machine you cannot expose. It is behind a home router, on a
changing address, and opening a port to the internet so a server can reach it is both fiddly and a
bad idea.

So the connection runs the other way. The **CITAR helper** (`citar-helper`, one program to download)
dials **out** to the CITAR server over a WebSocket, authenticates with a token and the machine's own
key, and serves model requests down that same connection. Nothing needs to be opened at the house,
and the server never needs to know where the helper is. Since 0.1.6 the helper is a Rust binary
speaking the helper protocol 2 (`crates/citar-helper/PROTOCOL.md`); the Python worker of 0.1.5 is
gone, and a 0.1.6 server tells one that connects to download the new helper
([Moving from 0.1.5](#moving-from-015)).

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

**The server runs the games; helpers serve models** (and, where the machine's owner lends its CPU, play
the lab's bot games as CPU work: [below](#lending-cpu-what-plays-on-it-and-for-whom)). The game engine,
the bots and the state of every played game stay on the server. A helper is a model endpoint with a
tunnel around it: it returns each completion exactly as its model server gave it, and the server reads
the tool calls in it, with the same parser it uses for every model.

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

Once the helper connects it appears on the **Servers** page like any other machine, and its models
can be used in seats and benchmark suites. Availability windows and budgets apply to it the same
way.

**What the hub checks, in order** (design/phase4-helper.md P4.5.1), once a first frame over 2 MiB
(far more than any hello) has been refused unread: the protocol (a 0.1.5 worker gets the one-line
"download the new helper" refusal and close code 4426, at no cost: no token lookup, no audit row, a
log line per address an hour); the address's limit of refused tokens (20 different ones in five
minutes from one address and the next hello is refused for a while, before any token is looked up;
one revoked token retried over and over counts once, so it never locks out the owner's other machines
behind the same address; the hellos of one address are checked one at a time, so a burst of them
sent together gets no more lookups than the limit); the token (if the server's database fails while
checking it, that is no refusal: the helper is closed with 1011 and tries again); and the machine key
(below). The address is the
connection's own unless `CITAR_BEHIND_PROXY` says a proxy in front is trusted for
`CITAR_TRUSTED_PROXY_HOPS` hops, as for every other limit. A token's last address and time are kept
and shown on the machine's Helper dialog.

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
| Its CPU runs (`GET /api/machines/{id}/jobs`: the lab's games it played, what it lends now, its trust counters) | Its owner and administrators; a member a group lends it to gets 403, any other member 404. Another member's experiment is named "a member's experiment" unless the caller may see it |
| Set its trust tier, mark it a verifier, withdraw its results, have its next results checked (`PUT /api/admin/machines/{id}/trust`, `POST /api/admin/machines/{id}/replicate`), pin the hardware classes it is priced by | Administrators; anyone else gets 404 |
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
| Helper | Its worker tokens (issue, revoke), how to run the helper, what the helper is (version, build, protocol) and the fingerprint of its machine key, with **Reset key…** | Its owner and administrators, on a helper machine |
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
using there). These take effect at once: the dialog shows what the prompt scheduler has in effect, and the machine's
connected helper is sent them at once (protocol 2's `settings`), which it answers with what it will actually do: each
figure the smaller of the site's and its owner's own ceiling. The slots changed in the machine's editor (**Requests at
once**) reach it the same way, and a change made from the command line or another process at the next keep-alive. The
server holds each answer to what the settings it answers asked, so a helper can never take more than the site gave it
(one that claims more is flagged `settings_overclaim` for administrators), and an answer that crossed a newer change
on the way is not taken for one. The dialog waits up to two seconds for the answer when something changed. **CPU sharing** (threads, CPU %, memory, the kinds of CPU work: lab games, ranking games, reference
games, and whether its games pause while its own model works) is sent and acknowledged the same way, and the
helper's job host plays the lab's games under it ([below](#lending-cpu-what-plays-on-it-and-for-whom)). The helper's
own local settings always win: the dialog shows what is **in effect** (the threads, the CPU share and the memory, each
limit **hard** or **soft** as the machine's operating system holds it, the memory each game may use, and what the
helper's own ceiling capped), or **"not lent by this machine's owner"** when the helper's own switch is off, and the
grants on the machine's group that lend it for **Lab (CPU work)**. The helper card on the Servers page shows the same
line.

**What the helper's report changes.** Each time it connects, the helper reports the machine's hardware (its scan, the
file given with `--hardware FILE`, or nothing with `--no-hardware`) and the models its model server lists, with which
are loaded. It scans the machine again every 6 hours, and sends the scan only when the machine changed; **Rescan now**
on the dialog's second tab asks it for a new scan at once, which the dialog shows when it comes (a few seconds; the
button needs the helper connected). The server **keeps the last report whatever the settings**, with its time, and the dialog shows how it differs from the machine's configuration. What it does with it
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
administrator **pins** it, here. Its **trust tier** (new, provisional, trusted, quarantined) follows how its CPU
results check out ([below](#verification-what-a-lenders-tier-means)), and administrators may set it. Both are shown to
the machine's owner. An
administrator also sees, here and on the card, when its helper has **over-reported** its generation time (more than the
server observed, by over 10% and a second, three times within an hour): its work is credited the time the server
observed whatever it reports.

## Revocation

Taking a machine away from its helper takes effect at once: **revoking the token** it connected with,
**disabling** the machine, **deleting** it, or **suspending its owner's account** closes the helper's connection
within a second of the change being saved, with a `shutdown` naming why (`revoked`, `disabled`, `deleted`,
`suspended`). Whatever it was generating fails (it is not retried), its next request is refused, the helper is told
why and stops instead of reconnecting (exit code 78 from a console; as a service it waits an hour), and a helper that
tries again with what was taken away is refused at its hello, or, if the change was saved while its hello was being
checked, as soon as it is connected. Revoking one token leaves a helper connected with another token alone. A change
made from the command line or another process is noticed at the next keep-alive (`CITAR_WORKER_PING_SECONDS`).

## The machine key

On its first run the helper makes a key for its machine. The first helper that connects with one of the machine's
tokens registers its key on the machine (trust on first use); a later connection with another key is refused
(`refused` `key`) until the machine's owner or an administrator presses **Reset key…** in the Helper dialog (or
`POST /api/machines/{id}/key/reset`), after which the next helper to connect registers its own. The dialog shows the
key's **fingerprint**, the 16 hex digits `citar-helper --version --home DIR` prints on the machine: if they differ,
someone else's helper used the token first, and the fix is to revoke the machine's tokens, reset the key and issue a
new token. A reset alone is not enough then: whichever helper connects next with a valid token registers its key. A
reset closes the helper's connection, if it has one (close code 1012), so that it reconnects and registers its key at
once. A reset is audited and never changes the machine's trust tier.

---

## For the person lending the machine

Run the **CITAR helper**: one file, with nothing else to install. The **Servers** and **Models** pages
offer the right download for your computer (Windows, macOS on Apple silicon, Linux x64 and ARM64); the
files are also on every [GitHub release](https://github.com/jprodgers/CITAR/releases/latest). Start it
with no arguments (double-click it on Windows) and it asks for the server's address and the token
once, remembers them in `helper.json` in your CITAR folder, and connects straight away every time
after that. `citar-helper setup` asks again, to give it a new token for instance.

On macOS and Linux, make the download executable first (`chmod +x citar-helper-*`; on macOS also
`xattr -d com.apple.quarantine citar-helper-macos-arm64`, since the build is not notarised).

If the server restarts, the helper reconnects by itself (after 2 s, then backing off to a minute).
Only a refused token or key, or a machine taken away, stops it for good.

`citar worker`, `citar setup --worker`, `install.sh --worker` and `install.ps1 -Worker`, which set up
the Python worker before 0.1.6, now say where to download the helper and exit with code 2.

### Moving from 0.1.5

A 0.1.6 server refuses the 0.1.5 worker with one message ("This server runs CITAR 0.1.6, which needs
the new CITAR helper (protocol 2)…") and close code 4426, and the other way round a 0.1.6 helper waits
for a 0.1.5 server to be upgraded, trying every 10 minutes: either side can move first, and in
between the machine is simply offline. The 0.1.5 worker run by hand logs the message and stops. **Run
as the system unit `citar setup --worker` installed** (`Restart=always`, `RestartSec=15`), it is
restarted into the same refusal every 15 s until the unit is disabled: cheap for the server (no token
lookup, a log line an hour) but noisy, so on such a machine:

1. `sudo systemctl disable --now citar-worker` (the 0.1.5 unit), or close the 0.1.5 worker;
2. download `citar-helper` for the machine from the Servers page of the server;
3. run `citar-helper setup`: it reads the old `helper.json` (its endpoint, models and quiet hours),
   asks for the new token, and asks whether to lend CPU (default no). A `worker.json` written by
   `citar setup --worker` works too, with `--config`.

A server started on a fresh database (as the VPS's 0.1.6 will be) has no 0.1.5 token: register the
machine again and issue a token there first.

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
| A hardware file to report (at most 256 KiB, else none is reported) | `--hardware FILE` | `hardware_file` | `CITAR_WORKER_HARDWARE_FILE` | none |
| Report no hardware | `--no-hardware` | | | |
| Report this machine's scan | (the default, without the two above) | | | scanned at start, every 6 hours, and on Rescan now |
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
own files under `helper/` there: `machine-key.json`, `scan.json` (its last hardware scan, so a restart within 6 hours
does not scan again unless the machine itself restarted), and `logs/helper.log` (rotated at 5 MB, three old logs kept; the token never appears in it). If another program holds the log open so that it
cannot be rotated (an editor, or a window following it), the log grows to 10 MB at most; the helper
then writes to its console only, and tries to rotate again every minute. It writes nothing outside
its home.

**The models it offers.** The helper lists the endpoint's models when it connects and every 30 s
after, and tells the server whenever the list, or which of them are loaded, changes. A listing that
fails or takes over 10 s is taken as a hiccup: only after three in a row (about 90 s) does it tell
the server that no model is loaded, and the next answer puts them back. With `models` (or `--model`)
it offers only those. It dials only the address you gave (`base_url`), and follows no redirect from
it.

**What it tells the server about the machine.** Unless told otherwise, the helper scans the machine
and reports the scan: the operating system and the host name; the CPU (its name, cores, threads,
sockets and highest clock); the memory (and its modules on Windows); the GPUs (from `nvidia-smi` for
NVIDIA cards: memory, driver, power limit and draw; on Windows also what the system lists, on Linux
`rocm-smi` and `lspci`, on macOS `system_profiler`); the physical disks and the volumes with their
free space; whether it is a laptop; its model server, when that is LM Studio or Ollama, with the
models it lists (only those `models` names, when you set it); and a first guess at its power draw, which the site may use as the machine's power
figures (Power is `manual` by default, so it does not). It is the same report the Servers page's
collectors (`collect_hardware.py`, `.ps1`, `.sh`) write, made by the helper itself. It is scanned
when the helper starts (a scan younger than 6 hours is reused, unless the machine itself restarted
since), every 6 hours after, and when the
machine's owner asks for one on the Servers page; the server is sent it only when something in it
changed (a volume's free space or a GPU's draw at that moment do not count).

To keep it to yourself:

- `citar-helper scan` prints exactly what would be sent, and `citar-helper scan FILE` writes it to a
  file, which you can edit and import on the Servers page instead. It reads the helper's own settings
  for the model server and `models` (`helper.json` in its home, or `--config`, then the
  `CITAR_WORKER_*` variables, the flags winning, as when the helper runs), and writes nothing else;
- `--hardware FILE` (`hardware_file`) reports that file instead of a scan, read again at each
  connection and on each Rescan now (UTF-16, as PowerShell 5's `>` writes it, is read too);
- `--no-hardware` reports nothing at all.

**Power samples.** Every 10 seconds the helper reads the GPUs' power draw (`nvidia-smi`, all cards
together, where the driver reports it) and the CPU's utilisation, and once a minute sends the
minute's readings. The server writes them as the machine's measured power, which costs energy by
what the machine drew rather than estimating it from its power figures (the machine's power figures
still say what its idle and full draw are). Where the GPU's draw cannot be read (an AMD or Intel
GPU, a Mac), the samples carry the CPU alone, the helper asks the driver again only once an hour,
and the server estimates the GPU's share from the machine's power figures while its model works. `--no-power` (`no_power`)
sends none, and the server keeps none for a machine whose owner turned its **Live sampling** off
(Hardware & power, on the Servers page).

**CPU lending is off unless you turn it on, on the machine itself.** The server's machine settings
can ask for CPU work, but the helper does none unless this machine's own switch is on: `setup`'s
question, `"cpu": {"enabled": true}` in `helper.json`, or `--cpu` for one run. A 0.1.5
`helper.json` has no `cpu` section, so a helper moved over from 0.1.5 lends no CPU until you say so,
and the server's machine dialog shows "not lent by this machine's owner". The `cpu` figures are a
ceiling: the helper takes the smaller of each of the server's figures and yours, and tells the
server what it will actually do.

### Lending CPU: what plays on it, and for whom

A machine whose owner lends its CPU (the switch above) plays the lab's games in its helper's job host, under the CPU
settings in effect. The server offers it, one game a thread:

- **its owner's own experiments**;
- **the experiments of members a grant on its group lends it to for Lab (CPU work)**: the grant must tick that
  purpose. A grant with nothing ticked covers every kind of *model* work but never lends the CPU, and an
  administrator's own experiments need such a grant too (an administrator's bypass of grants covers model work only);
- **the server's own ranking and reference games** (its collection), when the machine's CPU settings list those kinds.

It offers nothing while the machine is in its quiet hours, while a benchmark holds it, to a helper whose jobs build
differs from the server's (both sides must compute a game alike), or a game whose memory is more than the job host
gives each game; a game no machine can take waits on the Lab page, saying why. A member's experiment played under a
grant counts against the grant's budget like any other work, and each game is entered in the compute ledger with
**the machine's owner as the provider** and the experiment's owner as the user.

**Pauses.** With **"Pause its games while its own model works"** on (the default), the helper holds its games at their
next step while a request runs on its model, and lets them go when the request ends, so lending the CPU never slows
the model. A benchmark on the machine pauses them whatever that says, until it ends (if the helper's connection drops
meanwhile, its games go on, and the server pauses them again when it reconnects while the benchmark lasts). Paused
time does not count against a game's deadline (up to twice the deadline in all); a game the helper's own pauses held
past that is played elsewhere, not on that machine again for an hour.

**Results are kept until the server has them.** A finished game's result is signed with the machine key and kept in
the helper's `helper/results/` until the server acknowledges it; a helper that loses its connection sends it again
when it reconnects, and the server takes each result once. A helper stopped mid-game ends its games, and the server
plays them again elsewhere, offering that machine nothing until its helper connects again. A game the helper does not
answer within 30 seconds of its offer (it answers one waiting for its job host to start before then), or one silent
for five minutes, is taken back and offered again; a machine that keeps abandoning games is offered none for an hour.
A helper that refuses a game's spec, or fails a game as one no machine could play, is counted against its machine
(the server checked the game itself) and the game is played elsewhere. When you change the machine's CPU settings
while games play, the helper starts its job host again under the new figures once they finish, and tells the server
the memory each game now has.

**What the owner sees.** `GET /api/machines/{id}/jobs` lists the machine's recent runs (each game, its state, its CPU
and paused time, and its verification once settled), what it lends now and its trust counters.

### Verification: what a lender's tier means

The server does not take a helper's word for a game. Every result comes with a **claim** signed by the machine key: the
job, the run, the machine, the spec, the jobs build, the bots, the result's hash and the head of its chain of round
digests. The server checks the signature against the key the machine registered, the claim against its own records,
the result's hash (the claim's and the one sent beside the result must both be the result's own) and the chain (its
digests must fold to its head), and the result against its spec. A claim that does not hold is never accepted: the
game is played elsewhere, and the machine and its owner are quarantined (below). A result that arrives just as its
machine's key is reset, signed by the key that is gone, is no verdict on anyone: its game is simply played again.

Then, depending on the machine's **trust tier**, it may play the game again on a **verifier** to compare:

| Tier | Its results | How it moves |
|---|---|---|
| **new** (every machine starts here) | every one replayed on this server before it is accepted | 5 matching replays in a row make it **provisional** |
| **provisional** | about 10% replayed, chosen by the server's secret once the result is in (nothing about a game's offer tells the helper whether it will be checked); the rest accepted **unverified** | 50 verified matches, no mismatch, and no over-reported time within 30 days make it **trusted** |
| **trusted** | about 2% replayed, the rest accepted unverified | an administrator may mark it a **verifier** |
| **quarantined** | no CPU work at all, and no work of anyone else's but its owner's (a pooled prompt from another member is refused) | an administrator releases it: back to **new**, its counters reset |

A game is **verified** when its replay comes to the same result hash, chain head and round count. Its results line
says `verified`, `unverified` (accepted without a replay) or `reference` (played by this server itself). Until a
replayed game settles, its owner and the machine's owner see it **done** like any other; the replay is administrators'
alone.

**Who verifies.** This server's own job host (when its canary and self-check pass), and trusted machines an
administrator marked as verifiers. Another member's trusted machine is never enough: one person with two accounts could
otherwise vouch for their own. A new machine's games are replayed on this server only; when a remote verifier
disagrees with a game, this server plays it a third time and every machine that disagrees with it is quarantined. A
replay from a machine that stopped being a verifier while it played (an administrator unmarked it) checks nothing: the
game is replayed again by a verifier. To
keep this server from being swamped, a member's third new machine waits while two others have work in hand, new
machines' replays take at most every other run of this server's while other work waits, and a new machine whose
results wait for their replays is offered nothing more meanwhile (the Lab page says "waiting for a verifier").

**A mismatch** quarantines the machine, with the evidence for administrators: "same chain, different result:
tampering or an incompatible build", or the round where the two runs' chains parted (faulty hardware or an
incompatible build), or a bad signature, or a claim that does not hold. Every verdict but a parted chain also
quarantines the machine's **owner**: none of their machines is offered CPU work, a new one included, until an
administrator releases them. Then **every result the machine gave that was accepted unverified** since it was
registered (or last released) is **withdrawn**, not only the ones after its last verified match: its lines leave the
results files, its games are played again elsewhere, its ledger entries are rejected (no credits, and any budget they
counted against gets them back), and the ratings no longer count them. A machine that is quarantined has every match
it made as a verifier re-opened (even if an administrator unmarked it first): the games it verified are checked again
on this server, and a machine whose promotion those matches counted goes back to **new** if, without them, it falls
below its tier's threshold. Matches made after a machine's promotion are taken off its counters but never take the
tier back, and a tier an administrator set is never taken back by them.

**What this means for a lender:** an honest machine climbs on its own; a broken one (bad memory, an overclock) is caught
by its replays and quarantined without blame on its owner; a key reset never changes the tier. Model work is not
replayed: it is attributed to the machine, and over-reported generation time only keeps a machine from being
promoted.

### CPU limits, per operating system

CPU work runs in a **job host**: a child process of the helper (`citar-helper job-host`, which you
never start yourself) that plays the games under the limits in effect, so that a game's crash, a
game that runs out of memory or one that hangs never touches the helper's connection, and the
operating system has one process to hold to the limits. The server plays its own share of the
lab's games the same way, in a child of its own (`python -m citar.jobs host`), under its machine's
CPU settings. The limits are **hard** where the operating system can enforce them whatever the
games do, and **soft** where the job host can only lower its own priority or watch itself; the
helper reports which, and the server's machine dialog shows it:

| Operating system | CPU (`cpu_percent` of the whole machine) | Memory (`memory_mb`, every game together) | Reported |
|---|---|---|---|
| Windows | a Job Object with a hard CPU rate, at below-normal priority | the same Job Object's memory limit | hard, hard |
| Linux, with a user service manager (a desktop session, or `loginctl enable-linger`) | a `systemd-run --user --scope` with `CPUQuota` | the scope's `MemoryMax` (with no swap) | hard, hard |
| Linux otherwise (a server's system service, a container) | `nice 10` and the number of threads | `RLIMIT_DATA` | soft, hard |
| macOS | the background priority band (on Apple silicon, the efficiency cores) and the number of threads | a watchdog that ends the job host past the limit, checked twice a second | soft, soft |

`threads` is the most games at once everywhere: the job host refuses more. **Memory per game:**
`memory_mb` holds the job host's own memory too (its program, and on Linux without the scope
every game thread's whole stack, which `RLIMIT_DATA` counts as soon as it is made). So the job
host measures itself when it starts and offers each game what is left, less a tenth kept spare,
shared by the threads: with one thread and 200 MB, about 170 MB a game on Windows and under the
scope, and about 145 MB on Linux without it. The server only sends a machine the games that fit that figure, and the
job host refuses one that does not; give it more memory, or fewer threads, to take bigger games. A
job host whose memory leaves its games nothing refuses to start.

A game that needs more memory than its share all the same ends the whole job host: its games fail
as `memory` and are played again elsewhere (each game reports its memory as it grows, so the server
learns about how much it needed), and the helper starts a new job host for the next one. A job host
that cannot put its limits in place refuses to start rather than run without them. If you need
hard limits on a Mac, lend fewer threads.

Before it takes any work, the job host checks that this machine computes games as every other
machine does: a floating-point check (some libraries switch the processor to a mode that rounds
tiny numbers to zero) and a short reference game whose result is known. A machine that fails
either is offered no work at all, so it is never blamed for results it could not get right.
`citar-helper --self-test` runs the same checks and prints the helper's version, its build and its
jobs build (what its job host computes with; only machines of the same jobs build are compared).

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

or per weekday, `"Sat 23:00-08:00"`. During a window the helper takes no new work, and tells the server
when its window ends (protocol 2's `status`): the server's scheduler then treats the machine as in its
quiet hours, so pooled prompts are refused as temporary, with the time they may try again, instead of
being sent and refused one by one. Benchmarks and lab runs pause; a game somebody is playing warns rather
than stopping.

### When a machine is busy

Everything that asks a model something goes through the server's **prompt scheduler**, which keeps one queue per
machine (the design is `design/phase3-features.md` P3.3). A machine has as many slots as its **LLM slots** setting
(`max_concurrent`), capped by what its helper acknowledged (its own `--max-concurrent`); the scheduler sends a machine
no more requests than it has slots, so a busy machine **queues** work instead of refusing it, and nothing sleeps and
retries:

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
- **Cancel stops the generation.** When the server gives up a request (a game is closed or paused, a seat is changed,
  a prompt is cancelled or runs out of time), it sends the helper `cancel`. The helper drops its stream from the model
  server, which closes the connection (LM Studio, Ollama, llama.cpp and vLLM stop generating when their client goes),
  and then answers `cancelled`, which frees the slot at once: the next request goes straight away. Only a helper that
  does not acknowledge within 5 seconds leaves the slot *draining* (taken until its late answer, or the request's own
  timeout plus 15 seconds), and its machine is flagged `cancel_unacked` for administrators: a 0.1.6 helper that drains
  is misbehaving. The helper answers only over the connection the request went out on, so when that connection goes
  (it disconnects, or reconnects as a new connection) the slot is given back at once, and a helper that loses its
  connection stops every completion it had in flight. A `busy` refusal (a helper whose own slots are below the site's,
  before its acknowledgement arrived) is absorbed: the scheduler holds the machine at what it has in flight until a
  slot frees (at most five seconds when nothing else is in flight), and queues the request again.
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
- **Generation time is what the server saw.** A step is credited the smaller of the helper's own `gen_s` and the
  time from sending the request to receiving its answer; time spent queueing is recorded apart and never counted as the
  model's (nor against its turn's time limit, nor in its speed score). A helper that claims more than the server
  observed (by over 10% and a second) three times within an hour gets `overreports` in its machine's flags, with an
  audit row. Token counts are capped at the request's `max_tokens` and the model's context.

An administrator's `GET /api/status` lists what the scheduler holds for each machine.

### What the server can and cannot do

It can ask your machine to run a completion with one of the models you exposed, with at most five
sampling parameters (`temperature`, `max_tokens`, `top_p`, `seed`, `reasoning_effort`); the helper
drops any other, and nothing in a request changes which endpoint it dials, the key it sends or whether
it streams. It can ask it to scan its hardware again, and send it its settings, which your own
ceiling caps.

It can offer it lab games from the closed list the helper plays (lab, ranking and reference games of the bots compiled
into it), each a typed spec with no path, script or code in it, and replay some of its results elsewhere to check them.

It cannot read your files, run commands, see other things on your network, or use models you did
not list. The helper only speaks to the base URL you configured, and only forwards completion
requests to it; your quiet hours, your model list and your slots are enforced on your machine,
whatever the server asks.

Stop lending at any time: stop the helper, or revoke its token (or remove the machine) on the
Servers page; the server drops the connection at once. Neither needs the other's cooperation.

---

## Troubleshooting

**Connects, then disconnects.** The token is wrong or has been revoked, the machine is disabled
or removed, or its owner's account is suspended. The helper's log says which ("The server refused
this helper's token", or the server's reason). Issue a new token on the Servers page. "Another helper's
key" means the machine's key was registered by another helper: check the fingerprint and reset it
([The machine key](#the-machine-key)).

**`wss://` fails immediately.** The server's reverse proxy is not passing the WebSocket upgrade.
The nginx site and Caddyfile in `deploy/` do; a hand-written one often does not.

**Connects, but never gets work.** The helper offers models; the *server* decides which seat uses
which. Check that a seat or suite is configured to use that server's model, and that the machine
settings take model work. Also check the helper's own endpoint is reachable from its machine: its log
says when a listing of the models failed.

**Everything looks fine and completions fail.** The model is probably not loaded. A catalogue entry
is not the same as a model in memory.

---

## A note for operators

A helper is somebody else's computer doing work for your server. Two things follow:

- **Their availability is not yours.** Windows, quiet hours and a laptop being closed are all
  normal. Benchmarks that depend on a helper should expect to pause.
- **Their costs are theirs.** The registry records the machine's hardware and electricity so
  reports attribute the cost to the right place. Non-owners never see a server's cost
  configuration — the tariff, hardware prices and key backend are visible only to its owner.
