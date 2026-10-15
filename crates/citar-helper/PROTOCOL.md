# The CITAR helper protocol, version 2

This is the normative text of the protocol between the CITAR helper (`citar-helper`, this crate) and
a CITAR server's hub (`/ws/worker`). `design/phase4-helper.md` P4.2 is its summary. The fixtures in
`protocol/v2/` are its executable form, and both sides run all of them: this crate's tests from the
helper's side, `tests/test_helper_protocol.py` and the hub's conformance tests from the server's side.
A change to the protocol changes this file, the fixtures and both sides together.

The frame types are typed on both sides: `src/protocol/` here, `citar/server/helper_protocol.py` on
the server. The [frame table](#the-frame-table) below is parsed by a test on each side and compared
with the types that side's code knows.

Contents: [Transport and framing](#transport-and-framing) ·
[Values and checks](#values-and-checks) · [Versioning](#versioning) ·
[Close codes](#close-codes) · [The handshake](#the-handshake) ·
[Settings, catalog, hardware, power and status](#settings-catalog-hardware-power-and-status) ·
[Completions](#completions) · [Jobs](#jobs) · [Updates](#updates) ·
[Keep-alive and shutdown](#keep-alive-and-shutdown) · [The v1 refusal](#the-v1-refusal) ·
[The frame table](#the-frame-table) · [Frames, field by field](#frames-field-by-field) ·
[The conformance fixtures](#the-conformance-fixtures)

---

## Transport and framing

- **The connection.** The helper dials out to `wss://<server>/ws/worker` (`ws://` for a loopback or
  plain-HTTP server), as protocol 1 did, so a protocol 1 helper still reaches a server that can tell
  it to upgrade. The server never dials a helper.
- **One machine, one row, one connection.** One helper process, one WebSocket, one worker token and
  one machine key carry both of a machine's capabilities, model work and CPU work. A machine without a
  model endpoint runs the helper with `--no-llm` and lends CPU only. When a second connection
  presents the same machine, the server keeps the newest and tells the older `shutdown` `replaced`.
- **Frames** are UTF-8 JSON text frames. Each is one JSON object with:
  - `t`, its type: a non-empty string;
  - `ts`, the sender's Unix time in seconds, informational only (a number). Every sender writes it;
    a receiver accepts a frame without it.
- **Size.** A frame is at most 8 MiB (8,388,608 bytes of UTF-8). A receiver refuses a larger one
  without parsing it. A `job_result` is well under 1 MiB.
- **Depth.** Arrays and objects nest at most 64 deep, the frame itself being depth 1, counted on
  the text outside strings. A receiver refuses a deeper frame as `too_deep` before reading any of
  it, so a deep frame that is not JSON either is `too_deep` too.
- **Direction.** Each side accepts only the frame types the other side sends (the frame table's
  `From` column). A frame whose type the receiver does not accept from that sender, including a type
  it has never heard of, is **ignored**: logged once per type at debug level, never an error, never
  answered. This is what makes additions possible (see [Versioning](#versioning)).
- **Ids** are opaque to the helper, which only echoes them:
  - request ids, `id` in the completion frames: 1 to 64 characters from `A-Z a-z 0-9 _ -`; the
    0.1.6 server sends 16 lowercase hex digits (the scheduler's);
  - machine ids: the same alphabet and length (the server's `sv_…`);
  - job ids: `cj_` followed by 1 to 61 characters of that alphabet;
  - run ids: `cr_` followed by 1 to 61 characters of that alphabet.

## Values and checks

Both sides check every frame they receive against its type's fields, as the
[field reference](#frames-field-by-field) gives them, and refuse a frame that fails with a
**protocol error**: a code and the path of the offending value. The two sides agree on both, which
the invalid fixtures hold.

**Types** used in the field tables:

| Type | A JSON value that is |
|---|---|
| `int` | an integer from 0 to 2^53 − 1, unless a range is given (`int −100..100`, `int 1..16`) |
| `number` | a finite number from 0 to 10^15 (an integer is a number), unless a range is given |
| `bool` | `true` or `false` |
| `string ≤N` | a string of at most N characters (Unicode scalar values); `string 1..N` also refuses the empty string |
| `text` | a string of at most 4,000 characters, for people to read |
| `hex12`, `hex16`, `hex64` | a string of exactly that many lowercase hex digits |
| `base64:32`, `base64:64` | standard base64 (RFC 4648 §4, with padding) of exactly 32 or 64 bytes |
| `id`, `job`, `run` | the ids of [Transport and framing](#transport-and-framing) |
| `kind` | one of `lab_game`, `ranking_game`, `reference_game` |
| `"a" \| "b"` | exactly one of these strings |
| `2` | exactly that value |
| `object` | any JSON object, kept whole: data the protocol carries without reading (a hardware scan, a job spec, a result, a chat message) |
| `[T] ≤N` | an array of at most N values of type T (`[T] 1..N`: at least one) |
| `T \| null` | a T, or `null` |
| a name | a nested object described by its own table |

**Fields.** A field marked *optional* may be absent; when present it has its type (`null` is not
absent). Every other field is required. A receiver **drops fields it does not know** from the
protocol's own objects (the frame, and every object a field table describes) and keeps everything
inside an `object` value untouched. **Open** objects keep their other fields whole as well: a
request's messages and its params. **Closed** objects refuse unknown fields instead: in protocol 2
only the job claim (`job_result.claim`), because its bytes are signed. A sender writes no field a
frame does not have (the server's builders refuse one).

**Numbers on the wire.** A JSON integer literal outside −2^63 .. 2^64 − 1 is read as a floating-point
number (as `serde_json` reads it), so it is never an `int`. A number literal that does not fit a
64-bit float (`1e400`) is not JSON. `NaN` and `Infinity` are not JSON.

**Protocol errors**, checked in this order:

| Code | Path | When |
|---|---|---|
| `too_large` | `""` | the frame is over 8 MiB |
| `too_deep` | `""` | it nests more than 64 deep |
| `not_json` | `""` | it is not JSON (a syntax error, `NaN`, a number too large for a float, a lone surrogate in a string, a byte-order mark) |
| `not_object` | `""` | it is JSON but not an object |
| `no_type` | `t` | `t` is missing, not a string, or empty |
| `protocol` | `protocol` | a `hello` whose `protocol` is missing or is not exactly the integer 2 (checked before its other fields, so the hub can answer with [the v1 refusal](#the-v1-refusal)) |
| `missing` | the field's path | a required field is absent |
| `type` | the value's path | a value of the wrong JSON type (a string where an `int` belongs, a float for an `int`, `null` where it is not allowed) |
| `value` | the value's path | the right type, but out of range, too long, not one of the allowed strings, or not in its format (hex, base64, ids) |
| `unknown` | the field's path | a field a closed object does not have |

A **path** names the value from the frame's root: field names and array indexes joined by dots,
`machine.public_key`, `llm.models.0.key`, `outcome.chain.digests.12`. `ts` is checked like any
field: a `ts` that is not a number is `type` at `ts`.

**What a receiver does with a bad frame.** It logs the error (at most once a minute per frame type)
and drops the frame. Then:
- the hub, receiving a bad first frame (not a `hello`, or a `hello` that fails its checks other than
  `protocol`), closes the connection with code 4400 and sends no frame; the close's reason is the
  protocol error's code, a space and its path (`missing machine`, `value llm.models`; the code
  alone when the path is empty), cut to 123 bytes, and the helper logs it. A `hello` failing
  `protocol` gets [the v1 refusal](#the-v1-refusal);
- the helper, receiving a bad `request` whose `id` it can read, answers `error` code `bad_request`
  for that id; a bad `job_offer` whose `run` it can read, `job_reject` reason `bad_spec`;
- the hub, receiving a bad `done`, `error` or `cancelled` whose `id` it can read, fails that request
  as an `endpoint` error, not retryable, and frees its slot; a bad job frame whose `run` it can
  read, fails that run as a retryable `job_failed` `internal` would (the job is queued again);
- anything else bad is only logged.

**What the helper caps.** Values that come from the machine are cut to fit before they are sent,
so a correct helper never sends a frame the hub refuses for them:
- its models (`hello.llm.models`, `catalog.models`): at most 500, chosen in this order: the models
  of its allowlist, then the loaded ones, then the others by key; a model whose key is longer than
  200 characters is left out, and a label longer than 200 is cut;
- `machine.name` (its `--name`) to 120 characters and `machine.hostname` to 255;
- every `text` it sends (an `error`'s, a `job_failed`'s or an `update_status`'s `message`) to 4,000
  characters;
- what a `done` carries from its endpoint: at most 512 tool calls (the first by index; the rest are
  left out), each call's `id` and `function.name` to 200 characters and its `type` to 32, and
  `finish_reason` to 64. An answer whose text passes 8 MiB, or whose `done` would be larger than a
  frame (its text escaped as JSON), is not sent: the request gets `error` `endpoint`, not
  retryable.
An endpoint that lists hundreds of models (an `openai_compatible` service such as OpenRouter) is
therefore served as its first 500 by that order, not refused. Each cut is logged once.

## Versioning

- The helper's `hello.protocol` is `2`. A 0.1.6 server accepts exactly protocol 2 (`HUB_PROTOCOLS =
  (2,)`) and refuses anything else with [the v1 refusal](#the-v1-refusal).
- **Inside protocol 2 a change must be additive:** a new frame type, or a new optional field. Both
  sides ignore frame types they do not accept and fields they do not know, so an older peer is not
  disturbed by them.
- **Enumerations are closed.** A new value of an enumerated field (a reject reason, a refusal code, a
  job kind) is not additive: an older receiver would refuse the frame. It needs a new protocol
  version, or a new optional field the older side ignores.
- **Job specs are closed and typed** too (`citar-jobs`), because they decide what code runs and what
  it computes: a spec with a key the job host does not know is refused (`job_reject` `bad_spec`).
  In the frame, `job_offer.spec` is an `object`; the spec's own checks are the job host's.
- **A breaking change is protocol 3.** Whether a release then accepts both is that release's
  decision; 0.1.6's answer for protocol 1 is no. A server that does not speak a helper's protocol
  refuses it with the v1 refusal (or a `refused` `protocol`), which every helper from protocol 2 on
  reads as `refused` `protocol` and handles by waiting for an upgrade (its own, or the server's).

## Close codes

The server closes the WebSocket with one of these codes. The helper's reaction follows the frame
that came before the close (`refused`, `shutdown`, the v1 refusal); when a close comes with no such
frame, the code alone decides.

| Code | Sent when | The helper, when no frame said more |
|---|---|---|
| 4400 | the first frame was not a valid `hello` (the reason names the protocol error) | logs the reason and reconnects with backoff |
| 4401 | after `refused` `token` or `key` | a permanent refusal |
| 4403 | after `shutdown` `revoked`, `disabled`, `deleted` or `suspended` | a permanent refusal |
| 4408 | no `hello` within 30 s, or the connection went stale (70 s without a frame) | reconnects with backoff |
| 4409 | after `shutdown` `replaced` | a permanent refusal |
| 4426 | after the v1 refusal, or `refused` `protocol` or `build` | waits for an upgrade (10 minutes) |
| 4429 | after `refused` `rate` | retries after 60 s (`retry_after_s` when the frame came) |
| 1000, 1001 | the server stopped or restarted | reconnects with backoff |

Any other code, and a connection lost without a close, is a reconnect with backoff.

**Backoff:** 2 s, doubling to 60 s, each delay with up to 30% random jitter added; a `welcome` resets
it.

**Exit codes of the helper** (P4.3.9): 0 when the user or the service manager stopped it; 75 to be
restarted for an update; **78 for a permanent refusal, from a console run only**; anything else is a
crash. In service mode (`--service`) a permanent refusal does not exit: the helper idles, logs once,
and tries again every hour, so no service manager restarts it into a loop.

## The handshake

1. The helper connects and sends **`hello`** as its first frame, within 30 s.
2. The server checks, in this order:
   1. **the protocol:** a `hello` failing the `protocol` check, and any first frame that is a
      protocol 1 `hello` (it has `version: 1` and no `protocol`), gets the v1 refusal and close code
      4426: no token lookup, no audit row, no rate-limit count, at most one log line per address an
      hour;
   2. **the address's rate limit,** before any lookup: an address with too many distinct failing
      token hashes in the window (`worker_auth`) gets `refused` `rate` with `retry_after_s`;
   3. **the token:** unknown or revoked, its machine disabled or deleted, its owner not active:
      `refused` `token`; the failure counts toward the address's limit and is audited;
   4. **the key:** the first protocol 2 `hello` of a machine registers `machine.public_key` on its
      row (trust on first use, bound to a valid token); a later `hello` with another key gets
      `refused` `key` until the machine's owner or an administrator resets the key;
   5. **registration**, then `welcome`.
3. The server answers with exactly one of `welcome` and `refused`, and sends nothing else before it.
   After `refused` it closes the socket (4401, 4426 or 4429).
4. After `welcome` the helper sends `settings_ack` for the welcome's settings, then follows the
   welcome's `jobs`: it aborts every run of its `hello.running` that is not in `jobs.keep`, sends at
   once a `job_result` for each run in `jobs.resend`, and deletes every held result not in
   `jobs.resend`.

**`refused` codes and the helper's reaction** (part of the contract):

| `code` | Why | The helper |
|---|---|---|
| `token` | an unknown or revoked token, the machine disabled or deleted, the owner not active | a **permanent refusal**: it says to issue a new token (`citar-helper setup` asks for one); from a console it exits 78; as a service it idles, logs once and tries again hourly |
| `key` | the machine already has another key registered | a permanent refusal, as `token`, saying the owner must reset the machine's key on the Servers page |
| `protocol` | the server does not speak protocol 2 | waits: tries again every 10 minutes, logging that the server must be upgraded or its own update applied; never exits on it |
| `rate` | too many refused tokens from this address | tries again after `retry_after_s` (at least 1 s, at most an hour) |
| `build` | reserved for protocol 3 | as `protocol` |

The helper reads **the v1 refusal**, and a 0.1.5 server's answer to its `hello` (a protocol 1
`error` frame and close 4426), as `refused` `protocol`.

## Settings, catalog, hardware, power and status

- **`settings`** (server → helper, inside `welcome` and pushed whenever the machine's settings
  change): what the site asks of the machine, from `servers.machine_settings(id)`. A figure of `0`
  means "the helper's own ceiling".
- **`settings_ack`** (helper → server, after every `settings`, the welcome's included): what the
  helper will actually do, `effective`, and where its local ceiling won, `capped`. Local always wins
  (plan C3):
  - a number (`llm.slots`, `cpu.threads`, `cpu.cpu_percent`, `cpu.memory_mb`): the site's `0` is the
    ceiling; otherwise the smaller of the site's figure and the ceiling, `capped` naming it when the
    ceiling is the smaller;
  - `cpu.work`: the kinds both list, in the order of `kind`; `capped` names it when the site listed a
    kind the ceiling does not;
  - `cpu.enabled`: on only when the site and the machine's own switch both say on; `capped` names it
    when the site says on and the switch is off ("not lent by this machine's owner");
  - `llm.enabled`: on only when the site says on and the helper has a model endpoint (not
    `--no-llm`); `capped` names it when the site says on and the helper has none;
  - `cpu.yield_to_llm`: the site's choice, as given.
  The scheduler's capacity and the dispatcher's threads read `effective`, never the site's figures
  alone, and **never more than the site's either**: the hub clamps each acknowledgement to what it
  asked. It uses, for each number, the smaller of the site's figure and `effective`'s (a site `0`
  takes `effective`'s); for `cpu.work`, the kinds both list; and model or CPU work on only when the
  site and `effective` both say on. A correct helper's `effective` is never above the site's, so
  the clamp changes nothing for it; an acknowledgement that claims more (a modified or broken
  helper) is logged and flags the machine `settings_overclaim`, shown to administrators.
- **`catalog`** (helper → server): the endpoint's models, whenever the list or the loaded set
  changes; the helper polls its endpoint every 30 s. The models of `hello.llm.models` are the first
  catalog. A listing that fails or times out is not a change: the helper keeps its last catalog
  (in its next hello too) until three listings in a row have failed, and only then reports its
  models as none loaded (its allowlist's, or none).
- **`hardware`** (helper → server): the hardware scan, when it changed since the last one sent, and
  after every `rescan`. A scan younger than 6 hours is reused at connect; the helper scans again
  every 6 hours.
- **`rescan`** (server → helper): scan now, and send `hardware` even if nothing changed.
- **`power`** (helper → server, once a minute when it can measure anything): samples taken every
  10 s. They are measurements, written as the machine's power rows whatever its power mode.
- **`status`** (helper → server, on change, at most once every 5 s): the local quiet hours in force,
  the model requests in flight and the job runs running and paused. The server treats a machine in
  its helper's own quiet hours as one in the site's quiet hours. `quiet_until` is a whole minute,
  the same all through one stretch of quiet hours; quiet hours that cover every minute of the week
  never end, and are reported as ending at the start (Monday 00:00, local time) of the week after
  next: more than a week away, and changed once a week.

## Completions

- **`request`** (server → helper): one completion. `messages` is the whole OpenAI-format list, system
  prompt included (and in `json` tool mode the JSON protocol, which the server appends); `tools` are
  OpenAI function definitions, or `null`; `params` carries at most `temperature`, `max_tokens`,
  `top_p`, `seed` and `reasoning_effort`. The helper drops any other parameter (logged once per
  name) and decides the endpoint, the key and whether to stream: nothing in a request can change
  them.
- **`done`** (helper → server): the completion **as the endpoint returned it**, assembled from the
  stream when streaming. Nothing is parsed or repaired on the helper: the server parses tool calls.
  `gen_s` runs from sending the HTTP request to its last byte, `ttft_s` to the first token (`null`
  when no token came, or when the answer was not streamed, whose first token is not seen apart from
  the rest); `usage` is `null` when the endpoint gave none.
- **How the helper builds the request and reads the answer** (the endpoint is OpenAI's chat API at
  `<base>/chat/completions` on all four kinds):
  - the body is the request's `model`, `messages` and `tools` as they came (`tools` left out when
    `null` or empty: endpoints refuse an empty list), the five parameters it carries, and the
    helper's own `stream` (with `stream_options.include_usage` when streaming);
  - a streamed answer is read from its server-sent events up to `data: [DONE]`: `content` and
    `reasoning` (the endpoint's `reasoning_content`, or `reasoning` where it names it so) are the
    deltas' text concatenated, `null` when no delta carried any (an empty string when one carried
    `""`); tool calls are put together by their `index` (a delta with no index starts a new call
    when it has an `id`, and adds to the last call otherwise), keeping the first `id` and `type` and
    concatenating `function.name` and `function.arguments` (arguments the endpoint gives as JSON
    other than a string are written out as text); a call with no `type` is a `function` call;
    `finish_reason` is the last one given and `usage` the last one given that has both counts. Only
    choice 0 is read. `cancelled.tokens_out` counts the events that carried text;
  - an endpoint that answers a streaming request with JSON is read as unstreamed (`streamed`
    false); one that refuses a streaming request with tools (HTTP 400, 422, 500 or 501 whose body
    names streaming and tools or functions) is asked again without streaming, and every request
    with tools on that connection to the server then goes unstreamed (logged once: a cancel may not
    stop such a generation);
  - only a completion whose answer was read whole leaves its connection to the endpoint open, for
    the next completion (HTTP keep-alive, at most 30 s idle); a cancelled, timed-out or failed
    completion's connection is closed, so the endpoint sees its client go. `gen_s` and `ttft_s`
    count from sending the request that answered, once its connection is made.
- **The order of the helper's own refusals:** `unknown_model` (no endpoint, `--no-llm`, or a model
  outside the owner's allowlist), then `closed`, then `busy`; then the request runs. A `request`
  whose `id` is already in flight is ignored (logged): one answer per id.
- **`error`** (helper → server): the request did not complete. Codes:

  | `code` | Meaning | `retryable` |
  |---|---|---|
  | `busy` | the helper is at its local slots | true |
  | `closed` | the helper's local quiet hours | true |
  | `unknown_model` | the model is outside the helper's allowlist or not served | false |
  | `stopping` | the helper is shutting down | true |
  | `endpoint` | the local endpoint failed or refused (the message carries its status and the first 300 characters of its body) | as the endpoint's failure suggests: true when it could not be reached or its connection broke, and for HTTP 408, 425, 429 and 5xx; false for any other status, an answer that does not read, an error the endpoint reported in its answer, and an answer too large for a frame |
  | `timeout` | `timeout_s` passed (the helper caps it at 900 s) | false |
  | `bad_request` | the request frame failed its checks | false |

- **`cancel`** (server → helper): stop the request. The helper drops the response stream, which
  closes the connection to the endpoint (LM Studio, Ollama, llama.cpp and vLLM stop a streaming
  generation when its client goes), and only then answers **`cancelled`** with the generation time
  and the tokens received so far. A request that finished before the cancel arrived has had its
  `done` (or `error`): the cancel is ignored and nothing more is sent.
- The server frees the request's slot when `cancelled`, or a crossing `done` or `error`, arrives. A
  helper that does not acknowledge within 5 s (`CANCEL_ACK_SECONDS`) leaves the slot draining (P3.3.3)
  and the machine flagged `cancel_unacked`.
- **A lost connection cancels every completion in flight on the helper**, as a `cancel` would, with
  nobody to answer: the server has already failed those requests. After the reconnect the helper's
  slots are free, so the server's next requests are not refused `busy`.
- **A helper that stops** (Ctrl-C, its service manager) stops every completion in flight the same
  way and answers each `error` `stopping`, retryable, before it closes the socket, so the server can
  give them to another machine at once.

## Jobs

| Frame | Direction | Meaning |
|---|---|---|
| `job_offer` | server → helper | run this job; answer within `offer_ttl_s` |
| `job_accept` | helper → server | the run starts |
| `job_reject` | helper → server | the run will not start, and why |
| `job_progress` | helper → server | where the run is: every 10 s or 25 rounds; also the run's heartbeat |
| `job_result` | helper → server | the run's outcome, its signed claim |
| `job_result_ack` | server → helper | the result is committed (or was a duplicate): the helper may forget it |
| `job_failed` | helper → server | the run failed for a reason of the infrastructure |
| `job_abort` | server → helper | stop the run |
| `job_aborted` | helper → server | the run is stopped |
| `jobs_pause` | server → helper | pause every running game (a benchmark lease went live) |
| `jobs_resume` | server → helper | resume them |

A game that panics is a **result**, not a failure: its outcome is the crash record, which a replica
must reproduce. `job_failed` is for the infrastructure: the memory limit, the deadline, the job host
dying, the owner stopping CPU sharing, an internal error. The same frames, without `claim`,
`signature`, the acknowledgement and the offer's time to live, are the job host's own protocol on
its standard input and output (`citar-jobs`).

**The rules the frames carry:**
- **Results are acknowledged.** The helper writes each result to `helper/results/<run>.json` before
  sending it and deletes the file only on its `job_result_ack`. After a reconnect it lists the held
  runs in `hello.results`; the welcome's `jobs.resend` names those the server still wants (sent again
  at once) and the helper deletes the others. A second `job_result` for a run already settled gets
  another `job_result_ack` when its hashes are identical; when they differ it gets no
  acknowledgement and is a `claim` failure (P4.5.8).
- **Offers expire.** A run with neither `job_accept` nor `job_reject` within `offer_ttl_s` (30 s) is
  expired and its job offered elsewhere; a late `job_accept` gets `job_abort`.
- **A silent run expires.** A running run with no `job_progress` for 5 minutes while its helper is
  connected (the clock restarts at each connection and at the server's start), outside a pause the
  helper reported, is expired and its job queued again; a late `job_result` for it is acknowledged
  and dropped: not counted, not credited.
- **Re-offers back off** per machine and reject reason: 2 s doubling to 10 minutes. `busy` and
  `quiet` clear on a `status` that shows room; `build`, `kind`, `disabled`, `limits` and
  `unverifiable` on the next `settings_ack` or `hello`.
- **Paused time is reported** (`paused_s`, from `yield_to_llm` and `jobs_pause`) and extends the
  run's deadline on both sides, up to twice the deadline. The helper aborts a run past its deadline
  itself (`job_failed` `deadline`), not counting paused time; the server's own timeout, extended the
  same way, is the backstop.
- **A lost connection does not stop a run.** The next `hello` lists it in `running`, and the welcome
  says whether to keep it.
- **Frames name only the helper's own runs.** A job frame naming a run that was not offered to this
  connection's machine is dropped and logged.
- **Reject reasons:** `busy` (no free thread), `build` (not its jobs build), `kind` (not in its local
  work kinds), `quiet` (its local quiet hours), `disabled` (CPU lending is off on the machine),
  `limits` (the job host could not be started under the limits), `memory` (its memory per thread is
  below `mem_mb`), `bad_spec` (refused by the spec's checks, or its hash differs), `unverifiable`
  (the canary or the self-check failed).
- **Failure codes:** `memory` (the memory limit ended it), `deadline`, `host` (the job host died),
  `aborted_locally` (the owner stopped CPU sharing), `internal`.

## Updates

- **`update`** (server → helper, when the server learns of a manifest while the helper is
  connected): the signed manifest of the server's own version and its signature, as
  `welcome.update` carries them. `manifest` is the manifest file's text exactly as published, since
  the signature is over those bytes.
- **`update_status`** (helper → server): where the helper is with an update, shown on the Servers
  page: `available`, `downloading`, `verified`, `restarting`, `applied`, `failed`, `refused` or
  `unavailable` (a build that trusts no update key).

## Keep-alive and shutdown

- The server sends `ping` every `welcome.ping_s` seconds (20); the helper answers `pong`. The helper
  sends its own `ping` when it has heard nothing for 45 s, and the server answers `pong`, so the
  helper notices a dead link without waiting for the server. A connection is stale after 70 s
  without a frame: the server closes it with 4408, and the helper drops it and reconnects.
- **`shutdown`** (server → helper): the machine was taken away. Codes `revoked`, `disabled`,
  `deleted`, `suspended`, `replaced`. The helper aborts its jobs and requests and treats it as a
  permanent refusal: from a console it exits 78; as a service it idles and tries again hourly.
  `replaced` means another process holds the machine's token; an update's hand-off never causes one,
  because the old process closes its socket before the new one dials. The server closes with 4409
  after `replaced` and 4403 after the others.

## The v1 refusal

A 0.1.6 server **refuses** a protocol 1 helper; it does not serve it. Any first frame that is a
`hello` failing the `protocol` check gets one frame in the shape a 0.1.5 helper reads,

```json
{"t": "error", "ts": 1760000000.0, "id": "", "message": "This server runs CITAR 0.1.6, which needs the new CITAR helper (protocol 2). Download it from the Servers page of this server, or from https://github.com/jprodgers/CITAR/releases/latest, and run it with a token issued there."}
```

with exactly that message, then close code 4426. It is the handshake's first check and a single
branch, never a second code path. A 0.1.5 helper run by hand logs "The server rejected this worker:"
with the message and stops; one run as the 0.1.5 system unit is restarted into the same refusal
every 15 s until the unit is disabled (`sudo systemctl disable --now citar-worker`).

In the other direction, a 0.1.5 server answers a protocol 2 `hello` with its own protocol 1 `error`
frame ("This server speaks worker protocol v1…", with protocol 1's `refusal` and `retryable`
fields, which the helper reads and ignores) and close 4426. The helper reads both as `refused` `protocol`. In the frame table the v1
refusal is the `error` frame from the server.

## The frame table

Each frame type, who sends it, and what it is for. A test on each side parses this table and compares
it with the frame types that side's code knows.

| Frame | From | Purpose |
|---|---|---|
| `hello` | helper | the first frame: who the helper is and what it can do |
| `welcome` | server | the helper is accepted: its machine, settings, update and jobs |
| `refused` | server | the helper is not accepted, and why |
| `error` | server | the v1 refusal (protocol 1's shape), before a close with 4426 |
| `settings` | server | what the site asks of the machine |
| `settings_ack` | helper | what the helper will actually do |
| `catalog` | helper | the endpoint's models changed |
| `hardware` | helper | the hardware scan changed, or a rescan was asked for |
| `rescan` | server | scan the hardware now |
| `power` | helper | a minute of power samples |
| `status` | helper | quiet hours, requests and runs in flight |
| `request` | server | one completion |
| `done` | helper | the completion, raw |
| `error` | helper | the completion failed or was refused |
| `cancel` | server | stop a completion |
| `cancelled` | helper | the completion is stopped |
| `job_offer` | server | a job run offered |
| `job_accept` | helper | the run starts |
| `job_reject` | helper | the run will not start |
| `job_progress` | helper | the run's progress and heartbeat |
| `job_result` | helper | the run's outcome and signed claim |
| `job_result_ack` | server | the result is committed |
| `job_failed` | helper | the run failed (infrastructure) |
| `job_abort` | server | stop a run |
| `job_aborted` | helper | the run is stopped |
| `jobs_pause` | server | pause every running game |
| `jobs_resume` | server | resume them |
| `update` | server | a signed update manifest |
| `update_status` | helper | where the helper is with an update |
| `ping` | server | keep-alive |
| `ping` | helper | keep-alive, after 45 s of silence |
| `pong` | helper | the answer to the server's `ping` |
| `pong` | server | the answer to the helper's `ping` |
| `shutdown` | server | the machine was taken away |

## Frames, field by field

Every frame also has `t` (its type) and `ts` (*optional* to a receiver; a number). A field not
marked *optional* is required.

### `hello` (helper)

| Field | Type | Meaning |
|---|---|---|
| `protocol` | `2` | the protocol (checked first: [Values and checks](#values-and-checks)) |
| `token` | `string 1..512` | the worker token; never logged |
| `helper` | Helper | the helper's build |
| `machine` | Machine | the machine |
| `llm` | Llm | its model side |
| `cpu` | Cpu | its CPU side |
| `report` | Report | the hardware report |
| `running` | `[run] ≤1024` | the runs it is still playing from before a reconnect |
| `results` | `[run] ≤1024` | the runs whose results it holds unacknowledged |
| `updates` | `"auto" \| "notify" \| "off"` | its local update policy |
| `quiet_until` | `number \| null` | when its own quiet hours in force end, or `null` |

**Helper:** `version` `string 1..64` (its semver version); `build` `hex12` (its engine build id,
`citar_bot::build_id`); `jobs_build` `hex12 | null` (its jobs compatibility key, `null` for a build
without jobs, which is never offered one); `target` `string 1..64` (the Rust target triple); `label`
`string ≤200` (`git describe`, or `unknown`).

**Machine:** `hostname` `string ≤255` (cut by the helper); `name` `string ≤120` (the owner's
`--name`, cut by the helper); `os` `string 1..32` and `arch` `string 1..32` (Rust's
`std::env::consts` names: `windows`, `linux`, `macos`; `x86_64`, `aarch64`); `public_key`
`base64:32` (the machine key's ed25519 public key).

**Llm:** `enabled` `bool` (true unless `--no-llm`); `provider` `"lmstudio" | "ollama" |
"openai_compatible" | "openai" | null` (the endpoint's kind; `null` with `--no-llm`, which has none); `slots` `int 0..16` (its local
ceiling of concurrent requests, the configured `max_concurrent`; 0 with `--no-llm`); `stream`
`bool` (whether it streams from its endpoint); `models` `[Model] ≤500` (at most 500, chosen as
[What the helper caps](#values-and-checks) says).

**Model:** `key` `string 1..200` (the helper drops a model whose key is longer); `label` `string
≤200` (truncated by the helper); `context` `int | null` (the context length, when the endpoint
says); `loaded` `bool`.

**Cpu:** `enabled` `bool` (the machine's own switch: off unless its owner turned it on); `ceiling`
Ceiling (the local ceiling); `limits` Limits (how hard this OS holds each limit); `verifiable`
`bool` (the canary and the self-check passed); `bots` `[string 1..64] ≤64` (the bot versions compiled
in); `ruleset` `hex64` (the ruleset's id).

**Ceiling:** `threads` `int 0..1024`; `cpu_percent` `int 0..100`; `memory_mb` `int 0..16777216`;
`work` `[kind] ≤3`.

**Limits:** `cpu` `"hard" | "soft"`; `memory` `"hard" | "soft"`.

**Report:** `hardware` `object | null` (the scan in the `citar-hardware` format, or the owner's
file); `hardware_source` `"scan" | "file" | "none"`.

### `welcome` (server)

| Field | Type | Meaning |
|---|---|---|
| `protocol` | `2` | the protocol |
| `machine` | `{id: id, name: string ≤120}` | the machine row the token belongs to |
| `settings` | Settings | as the `settings` frame |
| `ping_s` | `int 5..300` | how often the server pings |
| `server` | `{version: string 1..64, build: hex12, jobs_build: hex12 \| null}` | the server's build |
| `update` | Update `\| null` | the signed manifest the server relays, or `null` |
| `jobs` | `{keep: [run] ≤1024, drop: [run] ≤1024, resend: [run] ≤1024}` | which of `hello.running` to keep and drop, which of `hello.results` to send again |

**Update:** `manifest` `string 1..65536` (the manifest file's text as published); `signature`
`base64:64`.

### `refused` (server)

| Field | Type | Meaning |
|---|---|---|
| `code` | `"token" \| "key" \| "protocol" \| "rate" \| "build"` | why |
| `message` | `text` | for the helper's log and its owner |
| `retry_after_s` | `number` *optional* | for `rate`: when to try again |

### `error` (server: the v1 refusal)

| Field | Type | Meaning |
|---|---|---|
| `id` | `string ≤64` *optional* | `""` |
| `message` | `text` | the refusal's text |
| `refusal` | `string ≤64` *optional* | protocol 1's, sent by a 0.1.5 server; ignored |
| `retryable` | `bool` *optional* | protocol 1's, sent by a 0.1.5 server; ignored |

### `settings` (server)

| Field | Type | Meaning |
|---|---|---|
| `llm` | `{enabled: bool, slots: int 0..16, allow_swaps: bool}` | model work |
| `cpu` | SiteCpu | CPU work |

**SiteCpu:** `enabled` `bool`; `threads` `int 0..1024`; `cpu_percent` `int 0..100`; `memory_mb` `int
0..16777216`; `work` `[kind] ≤3`; `yield_to_llm` `bool`. `0` in a figure means the helper's ceiling.

### `settings_ack` (helper)

| Field | Type | Meaning |
|---|---|---|
| `effective` | `{llm: {enabled: bool, slots: int 0..16}, cpu: SiteCpu}` | what the helper will do |
| `capped` | `[Capped] ≤7` | where its ceiling won |
| `limits` | Limits | how hard this OS holds them |

**Capped:** one of `"llm.enabled"`, `"llm.slots"`, `"cpu.enabled"`, `"cpu.threads"`,
`"cpu.cpu_percent"`, `"cpu.memory_mb"`, `"cpu.work"`, listed in this order.

### `catalog` (helper)

| Field | Type | Meaning |
|---|---|---|
| `models` | `[Model] ≤500` | as `hello.llm.models` |

### `hardware` (helper)

| Field | Type | Meaning |
|---|---|---|
| `scan` | `object` | the scan, or the owner's file |
| `source` | `"scan" \| "file"` | which |

### `rescan` (server)

No fields.

### `power` (helper)

| Field | Type | Meaning |
|---|---|---|
| `samples` | `[Sample] 1..360` | the minute's samples, oldest first |

**Sample:** `t` `number` (Unix time); `gpu_w` `number | null` (the GPUs' draw in watts, all GPUs
together; `null` where it cannot be read); `cpu_util` `number 0..1 | null` (the CPU's utilisation,
a fraction).

### `status` (helper)

| Field | Type | Meaning |
|---|---|---|
| `quiet_until` | `number \| null` | when its own quiet hours in force end, or `null` |
| `llm_in_flight` | `int 0..64` | model requests in flight |
| `jobs_running` | `int 0..1024` | job runs running |
| `jobs_paused` | `int 0..1024` | of those, paused |

### `request` (server)

| Field | Type | Meaning |
|---|---|---|
| `id` | `id` | the request |
| `model` | `string 1..200` | the model's key |
| `messages` | `[Message] 1..10000` | the OpenAI-format messages |
| `tools` | `[object] ≤512 \| null` | OpenAI function definitions, or `null` |
| `params` | Params | the sampling parameters |
| `timeout_s` | `number 1..86400` | how long the request may take (the helper caps it at 900 s) |

**Message:** an `object` whose `role` is a `string 1..32`; everything else in it is kept whole.

**Params:** an `object` with these fields, each *optional*: `temperature` `number 0..2`, `max_tokens`
`int 1..1048576`, `top_p` `number 0..1`, `seed` `int`, `reasoning_effort` `string 1..32`. Other
fields are kept in the frame, and the helper drops them (logged once per name).

### `done` (helper)

| Field | Type | Meaning |
|---|---|---|
| `id` | `id` | the request |
| `message` | `{content: string \| null, reasoning: string \| null, tool_calls: [ToolCall] ≤512}` | the endpoint's message, raw |
| `finish_reason` | `string ≤64 \| null` | the endpoint's, raw |
| `usage` | Usage `\| null` | the endpoint's token counts, or `null` |
| `gen_s` | `number` | seconds from the HTTP request to its last byte |
| `ttft_s` | `number \| null` | seconds to the first token, or `null` (no token, or not streamed) |
| `streamed` | `bool` | whether it streamed |

**ToolCall:** `id` `string ≤200` (as the endpoint gave it; may be empty); `type` `string ≤32`
(`function`); `function` `{name: string ≤200, arguments: string}` (the arguments' text, raw, even when
it is not valid JSON).

**Usage:** `prompt_tokens` `int`; `completion_tokens` `int`; `reasoning_tokens` `int | null`;
`cached_tokens` `int | null`.

### `error` (helper)

| Field | Type | Meaning |
|---|---|---|
| `id` | `id` | the request |
| `code` | `"busy" \| "closed" \| "unknown_model" \| "stopping" \| "endpoint" \| "timeout" \| "bad_request"` | why |
| `message` | `text` | for the server's log |
| `retryable` | `bool` | whether another try could succeed |

### `cancel` (server)

| Field | Type | Meaning |
|---|---|---|
| `id` | `id` | the request |

### `cancelled` (helper)

| Field | Type | Meaning |
|---|---|---|
| `id` | `id` | the request |
| `gen_s` | `number` | seconds generated before the cancel |
| `tokens_out` | `int` | tokens received from the endpoint before it |

### `job_offer` (server)

| Field | Type | Meaning |
|---|---|---|
| `job` | `job` | the job |
| `run` | `run` | this run of it |
| `kind` | `kind` | the job's kind |
| `spec` | `object` | the typed spec (`citar-jobs` checks it) |
| `spec_hash` | `hex64` | `blake3("CITAR-SPEC-V1\0" ‖ kind ‖ "\0" ‖ canonical bytes)` |
| `jobs_build` | `hex12` | the jobs build the job is for |
| `deadline_s` | `number 1..86400` | the run's deadline, paused time not counted |
| `mem_mb` | `int 0..16777216` | the memory a run is expected to need |
| `priority` | `int −100..100` | the job's priority |
| `offer_ttl_s` | `number 1..3600` | how long the offer stands |

### `job_accept` (helper), `job_result_ack` (server), `job_aborted` (helper)

| Field | Type | Meaning |
|---|---|---|
| `run` | `run` | the run |

### `job_reject` (helper)

| Field | Type | Meaning |
|---|---|---|
| `run` | `run` | the run |
| `reason` | `"busy" \| "build" \| "kind" \| "quiet" \| "disabled" \| "limits" \| "memory" \| "bad_spec" \| "unverifiable"` | why |

### `job_progress` (helper)

| Field | Type | Meaning |
|---|---|---|
| `run` | `run` | the run |
| `round` | `int` | the round being played |
| `of` | `int` | the game's last round |
| `elapsed_s` | `number` | wall seconds since it started |
| `cpu_s` | `number` | CPU seconds it used |
| `paused_s` | `number` | seconds it was paused |
| `peak_mb` | `number` | its peak memory, counted |

### `job_result` (helper)

| Field | Type | Meaning |
|---|---|---|
| `run` | `run` | the run |
| `outcome` | Outcome | what the run produced |
| `claim` | Claim | what the helper signs |
| `signature` | `base64:64` | `ed25519(machine key, "CITAR-CLAIM-V1\0" ‖ the claim's bytes)` |

**Outcome:** `result` `object` (the typed result, or a crash record); `result_hash` `hex64`; `chain`
`{head: hex64, rounds: int, digests: [hex64] ≤10000}` (one digest per round); `cpu_s`, `wall_s`,
`paused_s`, `peak_mb` `number`.

**Claim** (closed: an unknown field is refused): `v` `1`; `job` `job`; `run` `run`; `machine` `id`
(the id the welcome gave); `kind` `kind`; `spec_hash` `hex64`; `jobs_build` `hex12`; `bots` `[string
1..64] ≤64` (the sorted bot versions the spec names); `result_hash` `hex64`; `chain_head` `hex64`;
`rounds` `int`; `key` `base64:32` (the machine's public key).

Whether the claim agrees with the outcome, the run and the server's records is verification's
business (P4.4.4), not a frame check: a frame whose claim names another run is a `claim` failure of
that run, not a malformed frame.

### `job_failed` (helper)

| Field | Type | Meaning |
|---|---|---|
| `run` | `run` | the run |
| `code` | `"memory" \| "deadline" \| "host" \| "aborted_locally" \| "internal"` | why |
| `message` | `text` | for the server's log |
| `retryable` | `bool` | whether another run could succeed |

### `job_abort` (server)

| Field | Type | Meaning |
|---|---|---|
| `run` | `run` | the run |
| `reason` | `string ≤200` | for the helper's log |

### `jobs_pause` (server)

| Field | Type | Meaning |
|---|---|---|
| `reason` | `string ≤200` | for the helper's log: `benchmark` |

### `jobs_resume` (server)

No fields.

### `update` (server)

| Field | Type | Meaning |
|---|---|---|
| `manifest` | `string 1..65536` | the manifest file's text as published |
| `signature` | `base64:64` | its signature |

### `update_status` (helper)

| Field | Type | Meaning |
|---|---|---|
| `state` | `"available" \| "downloading" \| "verified" \| "restarting" \| "applied" \| "failed" \| "refused" \| "unavailable"` | where it is |
| `version` | `string ≤64 \| null` | the version concerned, or `null` |
| `message` | `text` | for the Servers page |

### `ping` and `pong` (both)

No fields.

### `shutdown` (server)

| Field | Type | Meaning |
|---|---|---|
| `reason` | `text` | for the helper's log |
| `code` | `"revoked" \| "disabled" \| "deleted" \| "suspended" \| "replaced"` | why |

---

## The conformance fixtures

Under `protocol/v2/`:

```text
frames/valid/<from>.<type>[.<variant>].json     one frame each, parsed by both sides
frames/invalid/<from>.<type>.<defect>.json      one defect each, refused by both sides
frames/ignored/<from>.<what>.json               frames both sides ignore without error
transcripts/<name>.json                         behaviour, played from both sides
schema.json                                     every frame's field table, as each side's code describes it
```

`<from>` is `helper` or `server`, the sender. `schema.json` is the field tables of this file as data
(`{"helper": {type: …}, "server": {type: …}}`, each type described as `{"int": [lo, hi]}`,
`{"string": [min, max]}`, `{"struct": [[name, type, optional], …]}` and so on): each side's tests
describe the tables their code checks and compare them with it, so a bound written differently on
one side is found even where no fixture reaches it. A change to a field table changes it
(`CITAR_BLESS_SCHEMA=1` and the helper's tests rewrite it from the helper's side).

- **A valid fixture** is `{"frame": F}` or `{"frame": F, "expect": E}`. Both sides parse `F` and
  serialize what they parsed again; the result equals `E` (by default `F`) as a JSON value, numbers
  compared by value. `E` differs from `F` only by the unknown fields the parse drops. The fixture
  named `<from>.<type>.json` is the type's **canonical example**.
- **An invalid fixture** is `{"frame": F, "code": C, "path": P, "why": "…"}`; or `{"raw": "text",
  …}` for a frame that is not JSON, or whose defect JSON cannot hold as a value (a number too large
  for a float); or `{"frame": F, "pad_bytes": N, …}`, F's JSON followed by N spaces, for a frame too
  large to commit. Both sides refuse it with protocol error `C` at path `P`. Each holds exactly one
  defect, so the two sides cannot disagree about which they found first.
- **An ignored fixture** is `{"frame": F, "why": "…"}`: a type the receiver does not accept from that
  sender. Both sides parse it as ignored, with no error.

### Transcripts

A transcript is one behaviour, as the frames and events both sides see:

```json
{
  "name": "completion-cancel-mid-stream",
  "about": "What the transcript shows, in a sentence or two.",
  "refs": ["P4.2.4"],
  "plays": {"helper": "4-04", "hub": "4-05"},
  "values": {"token": "…", "machine": "sv_0123456789abcdef", "req": "0123456789abcdef"},
  "setup": {"helper": {…}, "hub": {…}},
  "steps": [ … ]
}
```

- `plays` names the package from which each side plays it: the transcripts are written with the
  protocol and played by each side as its implementation arrives. A transcript whose behaviour only
  one side can show has `null` for the other side and says why in `one_side`: one that scripts a
  server other than the 0.1.6 hub (a 0.1.5 server, a future one refusing protocol 2, a close whose
  frame was lost, a broken frame), which only the helper's side can stand in for; one that scripts
  a helper misbehaving (a protocol 1 `hello`, a missing hello, a lost offer, a silent run, a
  duplicate result, a broken frame, an acknowledgement claiming more than the site asked), which
  only the hub's side can. A frame a correct peer never sends but the side under test must still
  handle (a request past the helper's slots, an offer past the dispatcher's checks) is sent through
  a test hook the transcript names with an act (`force_request`, `force_offer`), so both sides play
  it.
- `values` gives an example value for every `<id:NAME>` the transcript uses (the loader refuses a
  transcript that lacks one).
- `setup` gives each side's starting state in that side's own terms (the helper's configuration and
  home; the hub's machine, its settings and its database). The keys used are listed below; a player
  refuses a key it does not know, so a typo is never a silent pass.

**Steps** happen in order. Each step is an object with one action key, and optionally `within_s`
(it must happen within that many seconds of the previous step, on the player's clock), `note`,
`invalid` (a frame sent broken on purpose, refused with that protocol error code) and `lost`:

| Step | Helper side (the real helper, a fake server) | Hub side (the real hub, a fake helper) |
|---|---|---|
| `{"s2h": FRAME}` | the fake server sends FRAME, realized | the hub must send a frame matching FRAME next |
| `{"h2s": FRAME}` | the helper must send a frame matching FRAME next | the fake helper sends FRAME, realized |
| `{"close": {"by": "server" \| "helper", "code": C, "reason": R}}` | the fake server closes with C (and the reason R, when given), or the helper must close with C | the hub must close with C (and the reason R, when given), or the fake helper closes with C |
| `{"drop": {"by": "server" \| "helper"}}` | the connection is lost without a close (by the fake server), or the helper must drop it | the hub must drop it, or the fake helper drops it |
| `{"connect": {}}` | the helper must open a new connection | the fake helper opens a new connection |
| `{"wait": {"s": N, "quiet": B}}` | N seconds pass on the player's clock (a fake clock where it has one); with `quiet` true, the helper must send nothing but background frames and open no connection meanwhile | the same, for the hub |
| `{"endpoint": {…}}` | the fake model endpoint's next behaviour, or an expectation of it | skipped |
| `{"host": {…}}` | the fake job host's next behaviour | skipped |
| `{"act": {"side": S, "do": A}}` | when S is `helper`: something happens around the helper (its quiet hours begin); skipped when S is `hub` | when S is `hub`: something happens on the server (a seat submits a completion, the dispatcher queues a job, an administrator revokes the token); skipped when S is `helper` |
| `{"state": {"side": S, "expect": {…}}}` | checked when S is `helper` | checked when S is `hub` |

**A lost frame** (`"lost": true` on a frame step) is one the connection takes with it: the side
under test must still send it (the helper side checks the helper's `h2s` frame; the hub side, the
hub's `s2h` frame), and the scripted side never sends it, since its receiver never gets it. It is
how a transcript says "the connection dropped after the result was sent and before the hub had it".

**Background frames.** `ping`, `pong`, `status` and `power` come on their own clocks (keep-alive,
every change of the counts in flight, every minute), so a transcript cannot place each one:
- a player answers an unscripted `ping` (one no step expects) with `pong`, unless its setup says
  `answer_pings: false`;
- while the next step expects a frame of another type, the side under test's background frames are
  skipped;
- when the next step expects a background type, frames of that type that do not match are skipped
  until one matches, within the step's `within_s` (or the player's default wait);
- a `quiet` wait allows them, except those of the type the step after it expects (so "no `ping`
  before 45 s of silence" can be said): it asserts that no other frame comes and no connection
  opens.
Any other frame that arrives during a `wait` that is not `quiet` is kept, and the steps after the
wait match it in order.

**Setup keys**, the whole vocabulary (both loaders refuse any other):
- helper side: `config` (the helper's own settings, with these keys only: `max_concurrent`,
  `models`, `quiet`, `cpu` (`{enabled, threads, cpu_percent, memory_mb, work}`: its own switch and
  ceiling), `updates`, `no_llm`, `stream`, `no_power`), `clock` (`"fake"`: the player drives
  time), `home` (`{"results": [run]}`, results held from a previous connection; `{"running": [run]}`,
  runs still playing), `service` (true: run with `--service`), `endpoint` (`{"models": [Model]}`,
  the fake endpoint's models), `update_key` (`"test"`: a build trusting the run's throwaway update
  key, P4.3.8), `host` (the fake job host from the start, with these keys only: `verifiable`, false
  when its canary or self-check fails; `start`, `"limits"` when it cannot be started under the
  limits);
- hub side: `machine` (with these keys only: `settings`, the site's settings of the machine; `key`,
  `"registered"` or `"other"`, a key already on its row; `trust`, its tier), `token` (`"valid"` or
  `"revoked"`), `rate` (`{"failures": N}`, distinct failing tokens already counted for the
  address), `jobs` (`[{"run": run, "state": …}]`, runs the dispatcher already holds), `update`
  (`{"manifest": …}`, a manifest the server relays), `clock` (`"fake"`), `ping_s` (the hub's ping
  interval for the run), `local_executor` (false: the host's own executor takes no job, so a job
  queued again stays queued for the helper), `verification` (P4.5.8's settings for the run, over
  the defaults: `{"trusted_rate": 0}` samples no trusted machine's result), `answer_pings` (false:
  the fake helper answers no ping by itself).
Every transcript that reaches a `welcome` gives the site's settings (`machine.settings`, and the
same object in the welcome) and the helper's `max_concurrent` and `cpu`, and its `settings_ack` is
what those make by the rules of [Settings](#settings-catalog-hardware-power-and-status): nothing
depends on the machine a player runs on, and the hub side's fake helper acknowledges what a correct
helper would. Those a helper plays before 4-04 keep the site's model work off, so the
acknowledgement is the same whether the helper serves models yet or not.

**Endpoint steps** (helper side): `{"models": [Model]}` (the model list from now on);
`{"respond": {"stream": [delta, …], "delay_ms": N}}` (the next completion streams these
OpenAI-format deltas, N ms apart); `{"respond": {"json": body}}` (a non-streaming body);
`{"respond": {"status": S, "body": "…"}}` (an HTTP error); `{"respond": {"hang": true}}` (no answer);
`{"expect_request": PATTERN}` (the next HTTP request's body matches PATTERN), with `"absent":
[key, …]` the keys it must not have;
`{"expect_disconnect_within_ms": N}` (the client goes within N ms of the previous step);
`{"expect_no_tokens_after": "cancel"}` (no delta went out after the server's last `cancel` but the
one that may already have been on its way: measured from the cancel, since a delta can never go
out after the client went).

**Host steps** (helper side): `{"progress": {"round": R}}` (the fake job host reports this round);
`{"finish": "<golden:jobs/NAME>"}` (the run ends with that golden job's outcome); `{"crash_host":
"memory" | "other"}` (the job host dies); `{"hang": true}` (the run stops reporting).

**Actions** (`act.do`: a word, or an object whose one key is the word and whose value is its
argument), the whole vocabulary (both loaders refuse any other; a player refuses a word it cannot
yet do rather than skip it):
- helper side: `quiet_begins` (its local quiet hours begin, on its fake clock), `stop` (it is told
  to stop, as Ctrl-C or its service manager does);
- hub side: `submit_request` (a seat submits the completion the next `request` step shows, through
  the scheduler), `force_request` (the next `request` step's request is sent past the scheduler's
  own checks, slots, quiet hours and catalog, through a test hook: a request a correct scheduler
  never sends, for the helper's refusal of it), `cancel_request` (the seat cancels the request in
  flight, as closing its turn does), `drop_connection` (the hub's side of the socket is dropped
  without a close, through a test hook), `queue_job` (the dispatcher gets the job the next
  `job_offer` step shows, its cost table seeded so that the offer's figures are the step's, and
  offers it as it would), `force_offer` (the next `job_offer` step's offer is sent past the
  dispatcher's eligibility checks through a test hook: an offer a correct dispatcher never makes,
  for the helper's reject of it), `lab_stop` (the dispatcher's offers stop, as `citar lab stop`:
  running runs go on, nothing is offered), `abort_run` (the dispatcher aborts the run), `expire_run`
  (the dispatcher expires the run, as its deadline would), `{"set_settings": Settings}` (the
  machine's settings change on the site), `rescan` (the dialog's "Rescan now"), `reset_key` (the
  owner resets the machine's key), `revoke_token`, `disable_machine`, `second_connection` (another
  process presents the machine's token), `benchmark_start` and `benchmark_end` (a benchmark lease
  on the machine goes live and ends), `manifest_arrives` (the server learns of a signed manifest).

**State expectations** (`state.expect`'s keys), the whole vocabulary (both loaders refuse any
other):
- helper side: `in_flight` (completions in flight), `exit` (the process exited with this code),
  `held_results` (the runs whose results it holds), `running` (the runs it is playing), `downloads`
  (update downloads made), `logged` (a line of its log contains this text);
- hub side: `slot_free` and `draining` (the request's slot, and the slots draining), `flags` (flags
  the machine carries), `step` (the parsed step: `{"tool_calls": [names]}`), `requeued` (this
  request is queued again), `request` (`{"id", "failed": code}`: this request failed, with that
  cause, and is not retried on the machine), `llm_slots`, `cpu_threads`, `cpu_percent`,
  `memory_mb`, `work` and `cpu_enabled` (the figures the scheduler and the dispatcher use for the
  machine, after the clamp), `quiet` (the machine is in its helper's quiet hours), `models` and
  `loaded` (its catalog's keys, and the loaded ones), `last_report` (`"updated"`), `power_rows`
  (rows written), `key` (the key on its row), `audit` (audit actions written), `token_lookups`,
  `audit_rows` and `rate_count` (what a refused hello cost), `run` (`{"id", "state"}`, a
  `cpu_job_runs` state), `job` (the job's state), `result_lines` (lines in the results file for
  the job), `backoff` (the reject reason the dispatcher backs off for the machine),
  `mem_mb_raised`, `claim_failures`, `events` (ledger events for the run), `update_status` (the
  last one reported).

### The matcher language

A frame in a transcript is a **pattern**:
- a JSON **object** matches an object having every key the pattern has, each value matching
  (**open**: other keys are allowed, since protocol 2 only adds fields);
- an **array** matches an array of the same length, element by element (**closed**);
- any other literal (a string, number, boolean or `null`) matches an equal value, numbers compared by
  value;
- a **string of exactly one of these forms is a matcher**. A string `<kind>` or `<kind:…>` whose kind
  is one of these must be well formed, and the loaders refuse a transcript where it is not
  (`<int:5>`, `<id:>`); any other string, `"<think>"` included, is a literal:

| Matcher | Matches |
|---|---|
| `"<any>"` | any value, `null` included (the key must be present) |
| `"<bool>"` | `true` or `false` |
| `"<int>"`, `"<int:LO..HI>"` | an integer (not a boolean), within LO..HI when given (either end may be left out) |
| `"<number>"`, `"<number:LO..HI>"` | any number, within LO..HI when given |
| `"<string>"` | any string |
| `"<hex12>"`, `"<hex16>"`, `"<hex64>"` | that many lowercase hex digits |
| `"<base64>"` | a non-empty standard base64 string (with padding) |
| `"<id:NAME>"` | the first time NAME is seen, anything other than `null`, which is then **bound** to NAME; after that, only a value equal to NAME's binding |
| `"<golden:SET/NAME>"`, `"<golden:SET/NAME/FIELD>"` | a value equal to the golden set SET's entry NAME (or its FIELD), from the set's committed file (`jobs`: `crates/citar-jobs/golden/jobs.json`) |

Bindings live for the whole transcript, across both directions and every connection. A player may
bind names before the first step: the token it configured or issued (`token`), its own jobs build,
bot versions and ruleset id (`jobs_build`, `bots`, `ruleset`), the machine's id when it made the
row (`machine`). The job transcripts' hellos carry `<id:jobs_build>`, `<id:bots>` and
`<id:ruleset>`, so the hub side's fake helper presents the hub's own build and the dispatcher
offers it work. A fake clock starts where its player puts it: `values.quiet_until` is far in the
future, so quiet hours a fake helper reports are in force on any clock.

**Realizing.** The scripted side sends a pattern by realizing it: literals as written; `<id:NAME>` as
NAME's binding, or else `values[NAME]` (which then binds it); `<golden:…>` as the golden value; any
other matcher as the value the scripted side would put there itself (its own key, its own signature,
a result its job host computed), or, when it has none, the value at the same path in the frame type's
canonical example (`frames/valid/<from>.<type>.json`; an array index the example does not have reads
its first element), or else by the matcher's type: `<int:LO..>` and `<number:LO..>` their lower
bound, `<int>` and `<number>` 0, `<bool>` `false`, `<hexN>` N zeros, `<base64>` 32 zero bytes,
`<string>` `""`, `<any>` `null`. A realized frame passes its type's checks: the fixtures' own test
realizes every transcript frame with the canonical examples and parses the result, except frames a
transcript sends broken on purpose, which say so with `"invalid": CODE` on their step.
