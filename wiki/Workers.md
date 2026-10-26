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

**This guide, in order:** for the person running the server ([registering a machine](#for-the-person-running-the-server),
[helper updates and their signing](#helper-updates), [who may do what](#who-may-do-what-to-a-machine), [the Servers
page](#the-servers-page), [machine settings](#machine-settings-and-what-a-helpers-report-changes),
[revocation](#revocation), [the machine key](#the-machine-key)); for the person lending the machine ([getting and
running the helper](#for-the-person-lending-the-machine), [moving a 0.1.5 machine](#moving-from-015), [its
settings](#the-helpers-settings-citar-016), [updates](#updates), [running it as a
service](#running-the-helper-as-a-service), [lending CPU](#lending-cpu-what-plays-on-it-and-for-whom),
[verification](#verification-what-a-lenders-tier-means), [credits](#credits-for-cpu-work), [CPU limits per
OS](#cpu-limits-per-operating-system), [quiet hours](#quiet-hours), [when a machine is busy](#when-a-machine-is-busy),
[what the server can and cannot do](#what-the-server-can-and-cannot-do)); then [troubleshooting](#troubleshooting).

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

### Helper updates

The server relays the signed manifest of the CITAR helper of its own version to its helpers (design/phase4-helper.md
P4.5.9): fetched from the release's assets on GitHub (`CITAR_HELPER_MANIFEST_URL`, the default in server mode) or a
file (`CITAR_HELPER_MANIFEST_FILE`), at most hourly, off any request, and only when it verifies against the update keys
built into the server ([Configuration](Configuration#workers)) and names the server's own engine build (helpers
follow their server: a server built from changed sources relays nothing). A server built with no update key (every
0.1.6 build until the first signed release) fetches nothing at all and says so once in its log. A failed fetch keeps
the last good manifest. Every
helper's welcome carries it, and a new one goes at once to the connected helpers older than it; each helper decides by
its owner's `updates` setting ([Updates](#updates)). Each helper machine's card on the Servers page (and its Helper
dialog) shows its version beside the latest signed helper, "update available" when it is older, and where its helper
is with an update: downloading, applied, failed with why, refused with why, or that it does not update itself (a build
that trusts no update key: download the newer helper from the Servers page).

### Signing helper updates (the owner's one-time setup)

Updates are signed in CI by the release workflow's `sign-helper-manifest` job, with a key that lives only in a GitHub
Actions secret (the owner's choice of 2026-10-14). Until it is set up the job says so and signs nothing, and every
helper reports updates **unavailable**. To set it up, once, on a machine of your own:

1. **Make the key**, in a folder outside every git repository (the tool refuses one inside), from the repository's
   root: `cargo xtask keygen --out "$HOME/citar-update-key"` (Git Bash and PowerShell alike; in cmd,
   `%USERPROFILE%\citar-update-key`). Write the folder as a full path: Windows PowerShell 5.1 passes a `~` to a program
   as it is, so `~/citar-update-key` would name a folder inside the repository, which the tool refuses. It writes
   `citar-update-signing.key` (the private key: JSON with its `secret`) and `citar-update-signing.pub` (one line, the public key). The key file is its owner's alone before the
   secret is written into it: mode 0600 on Unix; on Windows `keygen` gives it a protected access list (you, SYSTEM and
   Administrators, nothing inherited from the folder, so a folder such as `C:\keys`, which every signed-in user may
   change, does not open it to them). Keep the private file somewhere safe and offline as well: it is the only way to
   sign updates helpers already trust.
2. **Store the private key in GitHub**, before the first tagged release: a tag's run names `helper-signing`, and
   GitHub makes a missing environment by itself, with no protection (if one was made that way, give it the two rules
   below before it gets the secret). The repository's Settings → Environments → New environment, named
   `helper-signing`; add a **required reviewer** (yourself) so every signing run waits for your approval; under
   "Deployment branches and tags" choose **Selected branches and tags** and add a **Tag** rule `v*`, so only a tag's
   run can reach the secret; add an environment secret `CITAR_UPDATE_SIGNING_KEY` whose value is the whole content of
   `citar-update-signing.key`.
3. **Commit the public key:** put the line of `citar-update-signing.pub` in `crates/citar-jobs/update-keys.pub` (one key
   a line, at most two) in a release commit. Helpers and servers built from it trust that key; the next tagged
   release's `sign-helper-manifest` job (after your approval) signs `citar-helper-manifest.json` and attaches it and its
   `.sig` to the release beside the binaries. The test suite holds that file to its format (public keys only, at most
   two, `tests/test_no_private_keys.py` and citar-jobs' `the_committed_key_file_holds_public_keys_only`), not to being
   empty, and the tests of updates sign with throwaway keys of their own, so the commit keeps CI green; `qa_phase4.py
   update` then expects the run's own helper to refuse the throwaway key's manifest rather than report updates
   unavailable.
4. **Check it** before the release, in a scratch folder `DIR` outside the repository (never in
   `tests/data/helper_manifest/`, whose `.sig` is a fixture the tests hold byte for byte: signing writes
   `MANIFEST.sig` beside the manifest). `cargo xtask helper-manifest --assets DIR --version V` over the built binaries
   (or a copy of `tests/data/helper_manifest/citar-helper-manifest.json` in `DIR`) and `cargo xtask sign-manifest
   --key "$HOME/citar-update-key/citar-update-signing.key" DIR/citar-helper-manifest.json` sign a manifest by hand the
   same way (the key file must be outside any repository), and `cargo xtask verify-manifest --pub
   crates/citar-jobs/update-keys.pub DIR/citar-helper-manifest.json` checks the signature against the committed key as
   a helper checks it (`scripts/ci/verify_manifest.py`, with the `cryptography` package, checks it a second way).
   [RELEASING.md](https://github.com/jprodgers/CITAR/blob/main/packaging/RELEASING.md), step 1, has these as
   commands to copy.
   Every pull request already proves this path with a throwaway key: `package.yml`'s `helper-manifest` job writes the
   manifest over the five builds, signs it with a key made on the runner, and verifies it both ways.

**What a release does with the two halves.** `sign-helper-manifest` runs only for a pushed `v*` tag, in
`helper-signing`, after your approval; a dispatched dry run skips it and never names the environment (its name is
an expression that is empty on a dispatch), so nothing waits and no environment is made. The job builds and installs
nothing: it runs the xtask binary `package.yml` built and used for the throwaway signature (its SHA-256 checked first)
through `scripts/ci/release_sign.py` on the runner's own Python, in one step whose only setting is the secret, so the
key never meets a compiler, a build script or a package installer. It holds the secret and the committed
`update-keys.pub` against each other:

| `update-keys.pub` | The secret | The release |
|---|---|---|
| empty | absent | a notice, and no signature: its helpers report updates unavailable |
| empty | present | **stopped**: commit the public key first (a signature nobody trusts) |
| a key | absent | **stopped**: its helpers would trust a key no release signs with |
| a key | present | signed, and the signature must verify against a committed line, else **stopped** ("the secret is not the committed key") |

Then `check-signature`, a job that holds no secret, checks the signature again with `scripts/ci/verify_manifest.py`
(the `cryptography` package, installed from hash-pinned wheels, sharing no code with the signer) against each
committed key. A stop comes before anything is published: PyPI, the image and the release page are untouched
([RELEASING.md](https://github.com/jprodgers/CITAR/blob/main/packaging/RELEASING.md), step 6, says what to fix and
re-run).

**Rotating the key:** make a new one, add its public line to `update-keys.pub` beside the old one in a release (two
slots), sign that release with the old key, then replace the secret and, a release later, drop the old line. A helper
trusts only the keys it was built with, so the step through a release that holds both is what lets old helpers follow.

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
| Helper | Its worker tokens (issue, revoke), how to run the helper, what the helper is (version, build, protocol) and the fingerprint of its machine key, with **Reset key…**, and its update line (below) | Its owner and administrators, on a helper machine |
| Its update | On a helper machine's card: its helper's version beside the latest signed helper this server relays, "update available" when it is older, and where the helper is with an update (downloading, applied, failed or refused with why, unavailable) ([Updates](#updates)) | Its owner and administrators |
| Machine settings… | Capabilities (LLM on or off, slots, model swaps for pooled prompts, CPU sharing), what its helper's reports may change (hardware, models and power, each auto or manual) with the last report and Apply, its hardware class and trust ([below](#machine-settings-and-what-a-helpers-report-changes)) | Its owner and administrators (pinning the class, the trust tier and the verifier mark: administrators) |
| Its trust | On a helper machine's card: its tier with its verified count and streak; to an administrator also its last mismatch with its evidence, its measured speed beside its CPU class, its verifier mark and the flags on it, with **Quarantine…** (withdrawing what it gave unverified, or not), **Release**, **Mark as verifier** (a trusted machine) and **Replicate…** ([Verification](#verification-what-a-lenders-tier-means)) | Its owner (the tier and counters) and administrators |
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
in [API](HTTP-and-tool-API#a-machines-settings)).

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
([Configuration](Configuration#credits-and-hardware-classes)): it follows the hardware details unless an
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

Run the **CITAR helper**: one file, with nothing else to install. There are five builds, under names
that never change:

| Your computer | The file |
|---|---|
| Windows (64-bit) | `citar-helper-windows-x64.exe` |
| macOS on Apple silicon (an M chip) | `citar-helper-macos-arm64` |
| macOS on an Intel processor | `citar-helper-macos-x64` |
| Linux x64 | `citar-helper-linux-x64` |
| Linux ARM64 (a Raspberry Pi 4 or 5 with a 64-bit system, an ARM server) | `citar-helper-linux-arm64` |

The **Servers** and **Models** pages offer the right download for your computer. A Mac's browser that
does not say which processor it has (Safari and Firefox never do) is offered both macOS downloads:
Apple menu → About This Mac says which (an Apple M chip, or an Intel processor). Every file is also on
every [GitHub release](https://github.com/jprodgers/CITAR/releases/latest), each with its SHA-256 in a
`.sha256` file beside it. Start the helper with no arguments (double-click it on Windows) and it asks
for the server's address and the token once, remembers them in `helper.json` in your CITAR folder,
and connects straight away every time after that. `citar-helper setup` asks again, to give it a new
token for instance. To keep it running with the machine (or from your sign-in), and after its
updates, install it as a service ([below](#running-the-helper-as-a-service)).

On macOS and Linux, make the download executable first (`chmod +x citar-helper-*`; on macOS also
`xattr -d com.apple.quarantine citar-helper-macos-*`, since the builds are not notarised).

If the server restarts, the helper reconnects by itself (after 2 s, then backing off to a minute).
Only a refused token or key, or a machine taken away, stops it for good.

`citar worker` and `citar setup --worker`, which set up the Python worker before 0.1.6, now say where
to download the helper and exit with code 2; `install.sh --worker` and `install.ps1 -Worker` download
it (the self-updating build for the machine, checked against its `.sha256` before anything is written)
into the bin folder, `~/.local/bin` or `%LOCALAPPDATA%\CITAR\bin` ([INSTALL.md](Installing#the-citar-helper) has
the one-liners). The Linux builds need glibc 2.28 or newer, as the game does.

**From a package manager:** `brew install jprodgers/citar/citar-helper` (macOS and Linux), `scoop install
citar-helper` (after `scoop bucket add citar https://github.com/jprodgers/scoop-citar`) or, once Microsoft accepts it,
`winget install JimmieRodgers.CITAR.Helper`. These install the helper's **packaged** build, from a versioned archive
with its notices: the same program, except that it never updates itself (its package manager does, [below](#updates)).
With Homebrew, run it as a service with `brew services start citar-helper`, not `citar-helper service install`, which
would record this version's Cellar path that the next `brew upgrade` removes.

### Moving from 0.1.5

A 0.1.6 server refuses the 0.1.5 worker with one message ("This server runs CITAR 0.1.6, which needs
the new CITAR helper (protocol 2)…") and close code 4426, and the other way round a 0.1.6 helper waits
for a 0.1.5 server to be upgraded, trying every 10 minutes: either side can move first, and in
between the machine is simply offline. The 0.1.5 worker run by hand logs the message and stops.

**The 0.1.5 unit's retries.** Run as the system unit `citar setup --worker` installed
(`/etc/systemd/system/citar-worker.service`, `Restart=always`, `RestartSec=15`), the 0.1.5 worker is
restarted into the same refusal every 15 s until the unit is disabled: cheap for the server (no token
lookup, a log line an hour) but noisy, and it would hold the machine's GPU slot for nothing. So that
unit is the first thing to go. The helper's own units are named `citar-helper.service` and restart
only on failure, never into a refusal ([below](#running-the-helper-as-a-service)).

**The runbook, for a machine moving to a 0.1.6 server** (the owner's own machines when the VPS moves,
design/phase4-helper.md P4.2.8):

1. **On the server:** deploy 0.1.6 ([DEPLOY.md](Deploying-a-server)). Register the machine on the Servers page
   (or keep its imported row) and issue it a token (Servers → the machine → Helper). A server started
   on a fresh database, as the VPS's 0.1.6 will be, has no 0.1.5 token: the machine needs a new one
   whatever the protocol.
2. **On the machine, stop the 0.1.5 worker:** `sudo systemctl disable --now citar-worker` where
   `citar setup --worker` installed it as a service (then `sudo rm
   /etc/systemd/system/citar-worker.service && sudo systemctl daemon-reload`); otherwise close it.
3. **Download the helper** for the machine (`citar-helper-<target>`, the table above) from the Servers
   page, and make it executable on macOS and Linux; or install it with `install.sh --worker`,
   `install.ps1 -Worker` or a package manager (above).
4. **Run `citar-helper setup`:** it reads the old `helper.json` (the endpoint, the models and the quiet
   hours carry over with their meanings), asks for the new token, and asks whether to lend this
   machine's spare CPU (default no). A `worker.json` that `citar setup --worker` wrote works too:
   `citar-helper setup --config /etc/citar/worker.json`, or copy its settings into `helper.json`.
5. **Run it** (`citar-helper`) and check the Servers page shows it online, with its version, protocol
   2 and the fingerprint `citar-helper --version` prints.
6. **Optionally install it as a service** (`citar-helper service install`, [below](#running-the-helper-as-a-service)),
   after stopping the copy started by hand: the newest connection wins, and the older helper is told
   it was replaced and exits.

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
| Updates ([Updates](#updates)) | `--updates` | `updates` | | `auto` (or `notify`, `off`; `notify` in a package manager's build) |
| Stream from the endpoint | | `stream` | | `true` |
| Send no power samples | `--no-power` | `no_power` | | |
| Run as a service (what every service definition runs) | `--service` | | | |
| Start in the background, under the Run supervisor (what Windows' Run value runs) | `run --detach` | | | |

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

### Updates

The helper updates itself from its server, never from anywhere else, and only to a release signed by CITAR's update
key (design/phase4-helper.md P4.3.8). The server fetches the signed manifest of its own version (the release's list of
helper binaries, each with its SHA-256 and size) and passes it on; your helper checks the signature again against the
keys built into it, so a server can pass on nothing the release key did not sign. Your `updates` setting decides what
happens then:

| `updates` | What the helper does with a newer signed release |
|---|---|
| `auto` (the default) | Downloads it into its home, checks its size and SHA-256, runs the new binary's `--self-test`, waits until nothing is in flight (taking work as usual for up to an hour, then no new work until what runs ends), closes its connection, keeps the old binary as `<name>.old` beside it, swaps, and comes back as the new version |
| `notify` | Reports it on the Servers page ("update available") and stops there: `citar-helper update apply` installs it when you choose |
| `off` | Ignores it (the Servers page says updates are off on this machine) |

To turn updates off: `"updates": "off"` in `helper.json`, or `--updates off`. A helper installed by a package manager
(Homebrew, Scoop, winget: a `packaged` build) defaults to `notify` and refuses `auto`: update it through the package
manager, which owns its binary (`brew upgrade citar-helper`, `scoop update citar-helper`, `winget upgrade
JimmieRodgers.CITAR.Helper`). Its `update check` says so: `policy: notify (a packaged build: its package manager
updates it)`, and with `"updates": "auto"` in its `helper.json`, `auto, which a packaged build refuses`.

What it checks, each a refusal the Servers page shows with its reason: the signature (a build that trusts no update
key reports updates **unavailable** and downloads nothing: **every 0.1.6 build until the owner's first signed release
ships a key**); a newer version only (never a downgrade, never the same one); a binary for this machine; `https` for
the download and every redirect, at most five; the size and the SHA-256 against the signed ones; and the new binary's
self-test (its version, build, protocol and target must be the manifest's, and its canary and self-check must pass).
The binary's folder must be writable by the helper's user: one installed by an administrator into a protected folder
says so and stays as it is.

The download waits in `helper/update/` in the helper's home, which the helper makes **its user's alone** (mode 0700;
on Windows an access list for that user, SYSTEM and Administrators only), so no other user of the machine can swap the
new binary between its checks and its installation; the copy put in place is checked against the signed SHA-256 once
more. If someone opens that folder to others, the update is refused with the command that closes it again (the same
rule as for the machine key). A download the network breaks off (or a server error, or a swap that fails) is reported
**failed** with when it is tried again: in 5 minutes, then 10, doubling up to every 6 hours while the helper runs; a
new release relayed starts afresh. A refusal (the signature, the version, the size or the SHA-256, your policy) and a
failed self-test are not tried again.

**After the swap.** Run by hand, the helper stays in your console and supervises the new binary: if the new binary
exits before its first welcome (or cannot be started at all, as when an antivirus quarantines it), the old one is put
back at once, connects again, and the Servers page says the update **failed** (with why); that binary is not tried
again until you delete `helper/update/failed.json` in the helper's home. Under a service manager the helper exits with
code 75 and the manager starts the new binary; one that does not come up healthy in three starts puts the old binary
back and exits 75 again, so the manager starts the old one. A start counts however early the new binary stops,
whether at a setting or a command line it does not take (a service run's command line counts too). The new binary
says **applied** once its server welcomes it.

By hand, in the helper's home (`--home` as the helper runs):

- `citar-helper update check`: this helper's version, the update keys it trusts, its policy, the last manifest its
  server relayed (and whether it is newer), and any update in progress or rolled back;
- `citar-helper update apply`: installs the relayed update now, checked and self-tested as `auto` would, keeping the old
  binary as `<name>.old`; restart the helper (or its service) to run it;
- `citar-helper update rollback`: puts `<name>.old` back, keeping the binary it replaces as `<name>.old`, so the same
  command puts the update back. The release it rolls away from is **held back** (written to
  `helper/update/failed.json`): with `updates: auto` the helper does not install it again at its next connection, and
  the Servers page shows it refused as rolled back on this machine. Delete that file (or run `update apply`) to take it
  again.

### Running the helper as a service

`citar-helper service install` keeps the helper running: started with the machine (or at your sign-in),
started again after an update's hand-off and after a crash, and never into a refusal. `service
uninstall` stops and removes it; `service status` says whether it is installed and running (exit 0 when
it runs, 3 when it does not). `--dry-run` prints exactly what `install` or `uninstall` would write and
run (the unit file, the plist, the `sc.exe` and `reg.exe` command lines, the home's access list) and
changes nothing.

A service reads its settings from its home's `helper.json` alone (a service has no command line of
yours and no variables of yours), so set the helper up first: run it once by hand, or `citar-helper
setup` (with `--home DIR` for another home). `install` refuses settings flags (`--server`, `--token`,
...) rather than drop them, and refuses a home whose settings would not start. Every definition runs
the helper with `--service`, its home's absolute path and the binary's: the binary stays where it is
(except for the Windows service, below), so keep it there; an update replaces it in place.

| Your system | `--user` (the default) | `--system` |
|---|---|---|
| Linux | a systemd user unit, `~/.config/systemd/user/citar-helper.service` (`Restart=on-failure`, `RestartSec=15`, `RestartPreventExitStatus=78`, `NoNewPrivileges=yes`), enabled and started with `systemctl --user`. It runs while you are logged in; to keep it running after you log out, run once `loginctl enable-linger $USER` (the helper prints the line and never runs it). Its output is in `journalctl --user -u citar-helper` | `/etc/systemd/system/citar-helper.service`, the same rules, with `User=` the account that ran `sudo`; run it as `sudo citar-helper service install --system --home DIR`, a home of that account's. Unelevated, it prints that command and exits 2 |
| macOS | a LaunchAgent, `~/Library/LaunchAgents/org.citar.helper.plist` (`RunAtLoad`, `KeepAlive` with `SuccessfulExit` false, `ThrottleInterval` 15), loaded with `launchctl bootstrap gui/<uid>`: it runs while you are logged in. Its output is in `helper/logs/launchd.log` in the home | not offered |
| Windows | the value `CITAR helper` under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, which starts `citar-helper run --detach --home …` at your sign-in (and `install` starts it now): a small supervisor runs the helper in the background with no window, starts it again at once after an update and after a crash with backoff (15 s doubling to 10 minutes), and stops when it stops (exit 0). Its log is `helper/logs/supervisor.log` beside the helper's | the Windows service `citar-helper`, started with the machine, running as the virtual account `NT SERVICE\citar-helper` (not LocalSystem). From an elevated prompt (Run as administrator): `citar-helper setup --home "C:\ProgramData\CITAR\helper"`, then `citar-helper service install --system`. Unelevated, it prints the elevated command and exits 2 |

**The Windows service in detail.** `install --system` copies the binary into
`%ProgramData%\CITAR\helper\bin` (updates, run by the service's account, replace it there; the file you
ran can go), and gives the home `%ProgramData%\CITAR\helper` an access list of SYSTEM, Administrators
and the service's account alone, owned by Administrators, with nothing inherited from `%ProgramData%`
(which would let every user of the machine read `helper.json`, which holds the token, and the machine
key). Under `%ProgramData%` any user may make folders and files, and owns what they make, so before
anything is made `install` checks every folder above the home, up to the drive's root: each must be
owned by SYSTEM, Administrators, TrustedInstaller or you, and none may let another account delete or
rename it or what it holds, or change its permissions or owner (a `%ProgramData%\CITAR` that another
user made before you install would let that user move the home aside and put their own, binary and
all, in its place, to run as the service). A missing folder above the home (`%ProgramData%\CITAR`
the first time) is made with its access list from its first moment: Administrators and SYSTEM, and
Users may read. The home is made the same way when missing, and taken over only when nothing in it
belongs to another account and no link, junction or hard link leads to it or lies in it; its `bin` is
made after it is secured. Otherwise `install` names the folder or file to remove and changes nothing.
Its recovery actions start it again 15 s after every failure, a non-zero exit code included (so
an update's hand-off, exit 75, restarts it), with no limit on how often. The service writes its
machine key for its own account: an administrator's `citar-helper --version --home ...` cannot read it
and prints no fingerprint, and the Servers page's Helper dialog shows it instead. `service run` is the
service's own entry, which the service manager starts; run from a console it says so and exits 2.

**What a service run does that a console run does not.** Exit codes are part of the contract:

| Exit code | Meaning | What the service manager does |
|---|---|---|
| 0 | stopped by you or the service manager | nothing (the Run supervisor stops too) |
| 75 | restart for an update ([Updates](#updates)): the new binary is in place | starts it (systemd and launchd after 15 s, the Windows service after 15 s, the Run supervisor at once) |
| 78 | refused for good (a console run only) | systemd does not restart it; a service run never exits with it |
| anything else | a crash, or a start that failed (a new binary's are counted, and the fourth puts the old one back) | starts it again (15 s; the Run supervisor backs off to 10 minutes) |

A service run **never exits on something permanent**: a token refused, a machine taken away, another
helper's key (the paragraph "When the server says no", below), and also settings it
cannot take or a machine key others can read. It says why once in its log, idles, and stops at once
when asked. A refusal from the server is tried again every hour. Settings it cannot take are read
again after 15 s, then after twice as long each time up to an hour, and at once when `helper.json`
changes, so a fixed file takes effect within seconds without a restart, and a file not readable yet
at boot costs seconds, not an hour. So no service manager restarts it into a loop every 15 s, as the
0.1.5 unit did. The one exception is an update in progress: a new
binary that cannot take its settings exits, so that its starts are counted and the old binary is put
back ([Updates](#updates)).

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
tier back, and a tier an administrator set is never taken back by them. Deleting a machine while its results wait
for their replays changes none of this: a mismatch found after the deletion still quarantines its owner and withdraws
everything the machine gave unverified.

**What this means for a lender:** an honest machine climbs on its own; a broken one (bad memory, an overclock) is caught
by its replays and quarantined without blame on its owner; a key reset never changes the tier. Model work is not
replayed: it is attributed to the machine (each step's ledger entry records its tier), and over-reported generation
time only keeps a machine from being promoted.

**For administrators**, `#/admin/verification` (linked from the users page) holds the settings (how many matches make
a machine provisional and trusted, the sample rates, how long over-reporting holds a machine back, how many new
machines of one owner play at once, and this server's share for new machines' replays, each bounded), the quarantined
machines with their evidence and the results withdrawn with them, the quarantined owners, each with **Release**, the
recent mismatches and withdrawals, the verifiers and the machines the credits' bounds or the class check flagged. A
machine's card and its machine settings dialog carry its trust and the controls; `GET /api/admin/jobs` shows the
verifiers' replays and the caps on new machines. A result that waited for its replay when the server was upgraded to
another jobs build (which no verifier plays) is accepted unverified at its next start when it was sampled; one of a
new machine's, one an administrator asked to check and one a verifier disputed are never accepted unchecked: each is
set aside with no verdict on anyone and no credits, and its game is played again.

### Credits for CPU work

Each game a machine plays is one entry in the compute ledger, given by the machine's owner and used by the
experiment's owner, priced at the machine's CPU class's rate per core-second. The seconds are the helper's own report,
so the server bounds them twice:

- **by what it observed**: never more than the time from the game's start to its result, plus 5% and a second (a game
  is one thread). A helper past that has its runs credited the bound and is flagged **over-reports its CPU**, which no
  honest helper can reach: it keeps the machine from being promoted to trusted for 30 days;
- **by the work itself**: never more than 1.5 times the game's **reference cost**, what it costs this server's own job
  host (the cost table's figure for its kind and size while its result waits, then the replay's own CPU once a replay
  reproduces it). A game held, or played on a slow machine, earns no more than that and is marked **CPU capped**; that
  is informational, since a slow honest machine is capped too, and never blocks a promotion.

Its replays also measure the machine: its **measured speed** is the median of the reference's CPU seconds over its own
on its replayed games long enough to time (a fifth of a second at least), each held between a twentieth and twenty
times the reference, shown to administrators beside its CPU class. Its games' deadlines scale by it, held between a
quarter and four times. A machine under half the median speed of its class is flagged **class suspect**: its class
prices it as faster than it is, or its helper over-reports; an administrator pins it to a fairer class (Machine
settings…, Class and trust). The median is of the class's other measured machines that did not over-report their CPU,
the trusted ones alone when there are any, so machines that under-report cannot set the bar for the rest. A game rejected by verification earns nothing, and any budget it counted against gets its
credits back.

### Model results and the global boards

Model work is not replayed (a model's answers cannot be checked by running them again), so the global boards (the model
scores, the diplomacy leaderboard) count a model's results from a member's machine only once that machine is
**trusted**: a game played on a machine that was not trusted when it ran (nor is since) is left out of every board but
its owner's own figures, where it is marked **unverified machine**, as are its benchmark run and its probe run; each
board says how many games it left out per model. A game knows each machine every model seat played on (read when the
seat's lease is made and again every minute while it lasts), so a seat that moved off an untrusted machine mid-game is
still left out, and a machine whose standing could not be read counts as unverified until the boards can read it.
When an administrator trusts the machine, its earlier results count too. The site's machines (the host, its online APIs) and an administrator's always count. A machine that serves models
but lends no CPU has no games to be promoted by: its owner asks an administrator, who can judge it by its results.

### CPU limits, per operating system

CPU work runs in a **job host**: a child process of the helper (`citar-helper job-host`, which you
never start yourself) that plays the games under the limits in effect, so that a game's crash, a
game that runs out of memory or one that hangs never touches the helper's connection, and the
operating system has one process to hold to the limits. The server plays its own share of the
lab's games the same way, in a child of its own (`python -P -m citar.jobs host`, or
`citar.exe jobs-host` in the Windows app), under its machine's CPU settings. The limits are **hard** where the operating system can enforce them whatever the
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
before trying again, so no service manager restarts it in a loop
([Running the helper as a service](#running-the-helper-as-a-service)). A server that does not speak the
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
  falls back), a soft one only warns ([Accounts](Design-accounts-and-sharing#budgets)). Every request a machine serves is in the
  compute ledger, given by the machine's owner and used by whoever's work it was
  ([Accounts](Design-accounts-and-sharing#the-compute-ledger-and-the-ratio)).
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

What a server (or whoever runs it) could try against a lender's machine, what stops it, and what is left
([SECURITY.md](https://github.com/jprodgers/CITAR/blob/main/SECURITY.md#helpers-and-shared-machines) holds the whole
threat model):

| A server could try to | What stops it | What is left |
|---|---|---|
| Make the helper run code, read files or reach your network | The helper accepts no command, script or code: CPU work is a closed list of games (lab, ranking and reference games of the bots compiled into it), each a typed spec with no path in it, run under limits; completions go only to the endpoint you configured, for the models you listed, with at most five sampling parameters (`temperature`, `max_tokens`, `top_p`, `seed`, `reasoning_effort`: the helper drops any other, and nothing in a request changes the endpoint, the key or streaming); it reads and writes nothing outside its home | A bug in the engine or a spec parser on hostile input: the frames and the specs are fuzzed, and the games run under the job host's limits |
| Wear your machine out | CPU lending is off until you turn it on at the machine; your own ceiling caps every figure the server sends; the limits hold hard where the operating system allows (Windows, Linux) and are marked soft where not (macOS); your quiet hours; its games pause while your model works | macOS holds the limits softly: lend fewer threads there |
| Learn more about your machine than it should | The hardware report can be a file you write or nothing (`--hardware FILE`, `--no-hardware`); power readings can be off (`--no-power`); the machine key's secret never leaves the machine | The prompts your model serves are seen by your machine, as before: lending a GPU means seeing what it computes |
| Use your machine for someone you did not lend it to | Every offer is checked against your groups' grants: CPU work only with a grant that lists Lab (CPU work), never one with no purposes | — |
| Install code on it as an update | Only a manifest signed by CITAR's release key, which no server holds, checked again by the helper; never an older or the same version; the download's SHA-256 and size checked, over `https` only; the new binary self-tests first and is rolled back if it does not come up; your `updates` setting (`notify`, `off`) holds | The release key's custody ([SECURITY.md](https://github.com/jprodgers/CITAR/blob/main/SECURITY.md#helpers-and-shared-machines)) |

It can ask it to scan its hardware again, and send it its settings, which your own ceiling caps. It can replay some of
its results elsewhere to check them ([Verification](#verification-what-a-lenders-tier-means)): what that costs a lender
is the time of the games replayed, never a game of theirs.

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

**The service does not start, or does nothing.** `citar-helper service status` (with the same
`--user`/`--system` and `--home`) says whether it runs; the helper's log in its home
(`helper/logs/helper.log`) says why it does not connect. A service whose settings are wrong runs and
idles, saying why once in that log: fix `helper.json`, and it reads it again within seconds. One whose
token was revoked idles and tries again every hour: give it a new token (`citar-helper setup --home
...`), then restart it (`systemctl --user restart citar-helper`, `launchctl kickstart -k
gui/$(id -u)/org.citar.helper`, `sc.exe stop`/`start citar-helper` from an elevated prompt, or
`service uninstall` and `install` again). On Linux a user service stops when you log out unless
lingering is on (`loginctl enable-linger $USER`). On Windows `install --system` needs an elevated
prompt, and says so.

**The update says unavailable, refused or failed.** *Unavailable*: this build trusts no update key (every build until
the first signed release): download the new helper from the Servers page. *Refused*: the reason follows (the same or
an older version, no binary for this machine, `updates: off`, a release rolled back here (by hand or after it failed:
delete `helper/update/failed.json` to take it again), a folder the helper may not write, an update folder others can
reach (run the command the message gives), a download whose size or SHA-256 is not the signed one). *Failed*: the
download broke off (it is tried again by itself, the message says when), the new binary failed its self-test, or it
did not come up and the old one is back; `citar-helper update check` says what the home holds, and the helper's log
says more.

---

## A note for operators

A helper is somebody else's computer doing work for your server. Two things follow:

- **Their availability is not yours.** Windows, quiet hours and a laptop being closed are all
  normal. Benchmarks that depend on a helper should expect to pause.
- **Their costs are theirs.** The registry records the machine's hardware and electricity so
  reports attribute the cost to the right place. Non-owners never see a server's cost
  configuration — the tariff, hardware prices and key backend are visible only to its owner.


---

*This page is generated from [`docs/server/WORKERS.md`](https://github.com/jprodgers/CITAR/blob/main/docs/server/WORKERS.md) and any edit made here will be overwritten. Corrections are welcome as a pull request.*
