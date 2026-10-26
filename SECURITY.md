# Security

## Reporting a vulnerability

Please report privately, through GitHub's private vulnerability reporting:

**<https://github.com/jprodgers/CITAR/security/advisories/new>**

Not in a public issue, and not in a pull request that fixes it — a fix in the open is a
notification to everyone, including people who would use it.

What helps:

- What an attacker can do, and what they need to already have.
- How to reproduce it. A `curl` command is ideal.
- The version: `citar --version`, and `citar doctor` if the configuration matters.

This is a one-person project, so: expect an acknowledgement within a few days, and a fix or a plan
within two weeks for anything serious. You will be credited in the release notes unless you would
rather not be.

## What is in scope

CITAR in **server mode** — a deployment other people sign in to. Specifically:

- Reading or changing another account's games, servers, reports or settings.
- Escalating from a member to a moderator or administrator.
- Anything that leaks an API key, a session, a worker token or a password hash.
- Authentication and session handling: sign-in, OAuth, invitations, password reset, CSRF.
- Anything a **worker token** allows beyond serving completions from the configured endpoint.
- Denial of service that a normal account can cause against everyone else.

## What is not

- **Local mode.** It binds to loopback and signs in whoever is at the machine, on purpose. That is
  not a bypass; it is a single-user application.
- **`--debug`.** It enables endpoints that reveal the map and grant gold. Never run it on a public
  server; that is stated in the configuration docs.
- **Anything needing administrator access already.** An administrator can reconfigure the server.
  That is what an administrator is.
- **Models behaving badly.** A model that plays terribly, loops, or says something odd is a quality
  issue, not a vulnerability. So is a model reading everything its own seat can see: that is what
  a seat is.
- **Unsigned Windows artifacts** and un-notarised macOS ones. Known and documented, with a SHA-256 and a build
  provenance attestation published for every file of every release ([The release chain](#the-release-chain)).

## The security properties CITAR tries to hold

Worth knowing if you are looking for holes — and worth knowing if you are reviewing a change.

**One gate.** `citar/auth/access.py` is the only place that answers "what may this viewer do with
this object". No route hand-rolls a check. A pull request that adds one is wrong even if it is
correct.

**404, not 403.** When a caller has no permission to *see* an object, the answer is 404. A 403
confirms the object exists, which turns guessing identifiers into enumeration.

**Watching is not playing.** A public spectator link never implies the right to act. Seat tokens
grant play on exactly one seat and nothing else.

**Moderators reach published content only.** There is no route from moderation to a private game.

**Route coverage is checked by a machine.** `scripts/audit_routes.py` walks every route, including
those inside included routers, and reports any that neither declares a gate nor appears in its list
of deliberately public routes with a reason. CI runs it with `--strict`. It exists because a
deployment probe once found an entire operator API readable anonymously, and because a later
version of the tool had a blind spot that hid exactly that class of bug.

**Secrets are not in the project folder.** API keys go to the OS credential store, an environment
variable, or an encrypted file outside the project. They are never returned to the browser, written
to a save, or included in a report. The environment file on a server is created mode 0600 rather than chmod-ed
afterwards, then given to `root:citar` 0640 so the service's group can read it and nobody else can.

**A public server names no path.** `/api/meta` is public (the lobby reads it before sign-in), so in server mode it
gives the version, the build's label and the MCP command by its program's name alone (`citar-mcp`, or the Windows
app's `citar.exe mcp`), never a folder or the interpreter; before 0.1.6 it gave the install's path and the Python's.

**A server runs from a release, sandboxed.** The service runs a release's own venv, root's and read-only to it, with
the state directory as its working directory and the only place it can write (`ProtectSystem=strict`); no source tree
is ever on its import path, and the job host it starts runs with `python -P`, so no file in the state directory can
stand in for a module. `citar-admin` runs every administration command as the service, with the same sandbox.

**The proxy hop count is explicit.** `CITAR_TRUSTED_PROXY_HOPS` decides how much of
`X-Forwarded-For` to believe. Too high and a client can forge its own address past the rate
limiter, so it is a setting rather than a guess.

**Local mode is a real session.** It mints a genuine session for a genuine account, and every check
runs exactly as it would in production. There is no branch anywhere that skips a permission test
because the server is local — which is what stops authorisation bugs from hiding during
development.

## Helpers and shared machines

From 0.1.6 a member's machine joins a server through the **CITAR helper**, which dials out to it. Two parties trust
each other a little: the lender trusts the server with their CPU and their model, and the server trusts the lender's
helper with games whose results go into everyone's lab, ratings and credits.

**What a worker token allows.** Serving completions from the endpoint the helper's owner configured, for the models
they listed, and running the server's closed list of CPU jobs (the lab's, ranking and reference games of the bots
compiled into the helper) under the owner's limits. Nothing else: a token never reaches an account's games, settings or
other machines. Anything a token allows beyond that is in scope.

**What a lender can rely on** (the helper's own guarantees, design/phase4-helper.md P4.3.10): it never runs a command, a
script or code it was sent; never reads or writes a file outside its home (and, when it updates itself, the folder its
own binary is in); never dials anything but its server, its configured model endpoint and, for an update, the signed
manifest's download address and the `https` redirects it leads to; never loads, unloads or swaps a model; never
changes a system setting. It runs only the
engine and bots compiled into it, on specs that are data. Its machine key is created mode 0600 (an owner-only DACL on
Windows) and refused at start when others can read it. CPU lending is off until the owner turns it on at the machine,
and the owner's own ceiling caps every figure the server sends.

**How the server checks what a helper says** (P4.4.4, P4.5.7, P4.5.8):

- Every result comes with a claim signed by the machine key (bound to its job, run, machine, spec, jobs build and the
  result's hash and chain), checked against the key the machine registered and the server's own records; the chain of
  round digests and the result's hash are recomputed by the server.
- Results are replayed on a verifier (this server, or a trusted machine an administrator marked one, never another of
  the same owner's): every result of a `new` machine, then a keyed sample (10% while `provisional`, 2% once
  `trusted`) decided only after a result is in, so a helper cannot tell which of its games will be checked.
- A mismatch quarantines the machine (and, for tampering, its owner) and withdraws everything it gave unverified since
  it was registered or released: results files, ratings, credits and budget settlements follow. A quarantined machine
  serves nobody but its owner, model work included. Deleting the machine while a result waits for its replay does not
  escape this: the owner is found from the deletion's record or the machine's ledger entries.
- CPU seconds are credited at most what the server observed (plus 5% and a second) and 1.5 times the job's reference
  cost; past the first, the machine is kept from promotion. A machine's measured speed is compared with its CPU
  class's.
- Model work cannot be replayed: it is attributed to its machine, its generation time bounded by what the server
  observed, and a member's machine's model results count on the global boards only once an administrator trusts it.

**The threat model:**

| Threat | Mitigation | Left over |
|---|---|---|
| A server makes the helper run code, read files or reach the lender's network | No command, script or code is ever accepted; jobs are a closed vocabulary of typed, bounded specs running compiled-in engine and bots; completions go only to the configured endpoint with whitelisted parameters and the model allowlist; nothing is read outside the home | A bug in the engine or a spec parser on hostile input: frames and specs are fuzzed, and the job host runs under limits |
| A server exhausts the lender's machine | Lending is off until turned on at the machine; the local ceiling caps every figure; hard limits where the OS allows, soft and labelled where not; local quiet hours; games yield to the lender's model; spec bounds and deadlines | macOS limits are soft |
| A server learns more about the lender than it should | Hardware reports can be a file or nothing; power readings can be off; the key's secret never leaves the machine | The prompts a lender's model serves are seen by the lender |
| A lender fakes results or poisons the ratings | Signed claims bound to the run; chains and result hashes recomputed; replication of every job of a `new` machine and samples after, decided only once a result is in and invisible to its owner until settled; verifiers only the server and administrator-marked machines; quarantine on any mismatch, of the owner too on tampering; withdrawal of everything unverified since registration or release | A patient cheater on a `trusted` machine contaminates about 50 jobs on average (2% sampling) before it is caught; all of them are withdrawn when it is |
| A lender inflates credits | CPU seconds bounded by the observed time and by 1.5 times the job's reference cost; generation time by the observed time; a class out of line with the measured speed flagged (the class's bar set by machines that did not over-report, trusted ones first); a measured speed held between a twentieth and twenty times the reference, and a deadline's between a quarter and four times, so over-reporting cannot stretch deadlines; rejected runs earn nothing and reverse their budget settlements; replays earn the lender credit but no one pays for them | A helper can be slow on purpose or hold results: it earns at most 1.5 times a job's reference cost and misses deadlines |
| A lender swamps verification or dodges quarantine with more accounts | Owner quarantine on tampering; at most two new machines per owner offered work at once; new machines' replays capped at half of this server's runs; a key reset keeps the tier | Many approved accounts can still occupy the verifiers' share; administrators see it and suspend accounts |
| A local user of a lender's machine reads its token or key | The machine key is owner-only and refused at start when others can read it; a `helper.json` (the token) others can read gets a warning with the fix; the Windows service runs as its own virtual account, `NT SERVICE\citar-helper`, not LocalSystem, with a home only SYSTEM, Administrators and that account can reach, installed only when no folder above that home is another account's to take over, the missing ones made with their access lists from the start (`service install --system`, design/phase4-helper.md P4.3.9) | An administrator of that machine can read anything on it |
| A lender serves a smaller model than claimed | Not verifiable; its events carry the machine's tier, its model results stay off the global boards until it is trusted, and administrators see over-reported time | — |
| Someone steals a machine's identity | Tokens hashed at rest and revocable; keys bound on first use and resettable only by the owner or an administrator; the token rate limit before any lookup | A token stolen before the real helper's first hello registers the thief's key; the fingerprint on the Servers page shows it, and a reset fixes it |
| A broken but honest machine produces wrong results | The canary and the self-check refuse jobs on a machine that computes differently; otherwise the same replays, the evidence naming the round where the chains parted and the owner not quarantined | — |
| A network attacker reads or alters traffic | TLS with the operating system's verifier; `--insecure` only for loopback and private hosts; `--ca-file` for a self-signed server | — |
| A member uses a machine they were not lent | Every offer is admitted by grant, with Lab (CPU work) listed explicitly for CPU work (an empty list never lends it) | — |
| A server pushes malicious or old code as an update | Manifests are verified against public keys compiled into the helper, whose private halves no server holds; never a downgrade or the same version; the download's SHA-256 and size checked against the signed ones, over `https` only, redirects included; the new binary self-tests before it is put in place, and is rolled back when it does not come up; the owner's policy (`off`, `notify`) holds locally, and a package manager's build never installs one | The release key itself (below) |
| Someone signs a malicious release | The signing key lives only in a GitHub Actions secret of a protected environment (`helper-signing`) whose required reviewer approves every signing run; two key slots allow a rotation | The owner's custody of that environment and their GitHub account |

**The update channel** (P4.3.8, P4.5.9, design/phase5-release.md P5.4.2). Each release publishes `citar-helper-manifest.json` (every helper binary's
download address, SHA-256 and size, and the release's version, protocol and engine build) and its signature,
`citar-helper-manifest.json.sig`: ed25519 over a domain string and the file's bytes as published. The private key never
touches a disk the project controls: it is the `CITAR_UPDATE_SIGNING_KEY` secret of the `helper-signing` environment,
read only by release.yml's signing job, on a pushed tag, after the environment's required reviewer approves. That job
builds and installs nothing: it runs the xtask binary package.yml built (its SHA-256 checked) through
`scripts/ci/release_sign.py` on the runner's own Python, in one step whose only setting is the secret, so the key never
reaches a compiler, a build script or a package installer. The secret and the committed public key are held to each
other (a secret with no committed key, a committed key with no secret, or a secret that is not the committed key stops
the release before anything is published), and a second job with no secret verifies the signature again with an
independent implementation. A dispatched dry run never names the environment. `cargo xtask keygen`,
`helper-manifest` and `sign-manifest` refuse a key file inside any git working tree. The public key is compiled into the helper (and the server's
extension) from `crates/citar-jobs/update-keys.pub`, which holds at most two keys and **is empty until the owner's
first signed release**: until then every helper reports updates "unavailable" and never downloads anything. A server
fetches the manifest of its own version, relays it only if it verifies against the same keys, and pushes it only to
older helpers; the helper checks it again, so a compromised server can relay nothing the release key did not sign.
The helper downloads into its home, checks the size and the SHA-256, runs the new binary's `--self-test` (its version,
build, protocol and target must be the manifest's, and its canary and self-check must pass), waits until no work is in
flight, closes its connection, keeps the old binary as `<name>.old`, and swaps; a new binary that exits before its
first welcome is swapped back (by the console run's supervisor at once, or by its own start count under a service
manager, which counts a start however early it stops), and the old one reports the failure and does not take that
binary again; nor does it take a release its owner rolled back by hand. The download waits in a folder of the helper's
home that only its user can reach (mode 0700, or an access list for that user, SYSTEM and Administrators), refused
with the fixing command if anyone opens it to others, and the copy put in place is checked against the signed
SHA-256 again, so another local user cannot swap the binary between its checks and its installation. A helper a
package manager installed (Homebrew, Scoop, winget: the `packaged` build) never updates itself and refuses `auto`. A server whose
extension trusts no update key fetches no manifest at all, and one relays only a manifest of its own engine build.
`cargo xtask keygen` writes the private key owner-only (mode 0600, or that access list on Windows) before the secret
is in it. Development and test builds (`dev-guard`, any test feature) also take `http` downloads, but only from this
machine, as they dial nothing else.

## The release chain

What a release is made of, and why each file can be traced to the commit and the run that built it
(design/phase5-release.md P5.3 to P5.12; [packaging/RELEASING.md](packaging/RELEASING.md) is the procedure):

- **One build, tested before it is published.** `.github/workflows/package.yml` builds every file a release ships (the
  five wheels and the sdist, the Windows installer and zip, the ten helper builds, both images), each on its own
  platform, and tests each one there, on every pull request. `release.yml` builds nothing: on a tag it calls the same
  workflow and publishes exactly what it assembled, one version, one build id and one label across every file.
- **Publishing only from a tag push.** Every job that publishes, holds a secret or writes anywhere runs only for a
  pushed `v*` tag; a dispatch is a dry run that publishes nothing and names no environment.
  `scripts/ci/check_workflows.py`, on every pull request, holds every workflow to that and to the rest of these rules:
  every action pinned to a commit SHA (Dependabot moves the pins), and every image a workflow builds or runs in pinned
  by digest; called workflows get no secrets; nothing is installed or built in a step that reads a secret or a job
  that holds a write permission; each publishing job's permissions and order.
- **Signed and attested before anything is public.** The helper manifest's signature passes both verifiers, then
  `attest-files` makes build provenance for every file (`actions/attest-build-provenance`, Sigstore's public log)
  before PyPI, the registry or the release page sees anything. `SHA256SUMS` is written once, in a job that holds
  nothing, and the bytes attested are the bytes published. PyPI's trusted publishing adds its own attestations.
- **The image pushed is the image tested**, saved by the run that built and checked it and loaded by its id; its
  provenance is attested after the push. `:latest` moves last, after the release is published.
- **The GitHub release is a draft until it is whole**, so `releases/latest/download/` never points at a release
  without its helpers.
- **The deploy checks provenance before a server sees a file.** `scripts/deploy/bundle.py` accepts a wheel only with
  release.yml's attestation for that tag, made on GitHub's runners from the same commit as the deploy scripts.

Anyone can check a file the same way: `gh attestation verify FILE --repo jprodgers/CITAR --signer-workflow
jprodgers/CITAR/.github/workflows/release.yml --source-ref refs/tags/vV --deny-self-hosted-runners`
([docs/INSTALL.md](docs/INSTALL.md#checking-a-download)). Third-party licences travel with every binary
([NOTICE.md](NOTICE.md)).

## Supported versions

Pre-1.0, only the latest release. Security fixes go out as a patch release, and the advisory says
what to do if you cannot upgrade immediately.

| Version | Supported |
|---|---|
| 0.1.6 | yes, while it is the latest |
| 0.1.5 and earlier | no: upgrade. A 0.1.6 server refuses 0.1.5's worker, whose replacement is the CITAR helper |
