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
- **Unsigned Windows artifacts.** Known and documented, with checksums published for every release.

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
to a save, or included in a report. The environment file on a server is mode 0600 and created that
way rather than chmod-ed afterwards.

**The proxy hop count is explicit.** `CITAR_TRUSTED_PROXY_HOPS` decides how much of
`X-Forwarded-For` to believe. Too high and a client can forge its own address past the rate
limiter, so it is a setting rather than a guess.

**Local mode is a real session.** It mints a genuine session for a genuine account, and every check
runs exactly as it would in production. There is no branch anywhere that skips a permission test
because the server is local — which is what stops authorisation bugs from hiding during
development.

## Supported versions

Pre-1.0, only the latest release. Security fixes go out as a patch release, and the advisory says
what to do if you cannot upgrade immediately.
