"""Protocol 2 from the server's side (package 4-01): ``citar/server/helper_protocol.py`` against the shared fixtures
in ``crates/citar-helper/protocol/v2/``, which the helper's own tests read too.

- Gate 1: every valid frame parses and serializes back to its expectation; every invalid frame is refused with its
  fixture's code and path; every ignored frame is ignored.
- Gate 2: PROTOCOL.md's frame table is exactly the frame types this side knows.
- Gate 3: 2,000 random mutations of the valid frames raise nothing but ``ProtocolError``.
- Gate 4: every transcript loads, every frame in it realizes into a frame that passes its checks, and each matcher
  accepts and refuses what it should. The hub can drive every transcript it plays: each frame it starts has its act,
  and each settings_ack is what a correct helper sends for the transcript's settings (4-01's fix round).
"""
import json
import random
import re
import unittest

from citar.server import helper_protocol as HP
from tests import helper_fixtures as F


def _parse(sender: str, text: str):
    return HP.parse(text, sender)


class FrameFixtures(unittest.TestCase):
    def test_every_valid_frame_parses_and_serializes_back_to_its_expectation(self):
        fixtures = F.frame_fixtures("valid")
        self.assertGreaterEqual(len(fixtures), 40)
        for f in fixtures:
            with self.subTest(f.name):
                got = _parse(f.sender, f.text)
                self.assertIsInstance(got, dict)
                back = json.loads(json.dumps(got))
                self.assertTrue(F.json_eq(back, f.expect), f"{f.name}:\n  got    {back}\n  wanted {f.expect}")

    def test_every_invalid_frame_is_refused_with_its_code_and_path(self):
        fixtures = F.frame_fixtures("invalid")
        self.assertGreaterEqual(len(fixtures), 60)
        codes = set()
        for f in fixtures:
            with self.subTest(f.name):
                with self.assertRaises(HP.ProtocolError) as caught:
                    _parse(f.sender, f.text)
                self.assertEqual((caught.exception.code, caught.exception.path), (f.code, f.path),
                                 f"{f.name}: {caught.exception}")
            codes.add(f.code)
        self.assertEqual(codes, set(HP.ERROR_CODES), "every code has an invalid fixture")

    def test_every_ignored_frame_is_ignored(self):
        fixtures = F.frame_fixtures("ignored")
        self.assertTrue(fixtures)
        for f in fixtures:
            with self.subTest(f.name):
                self.assertEqual(_parse(f.sender, f.text), HP.Ignored(f.frame["t"]))

    def test_every_frame_type_has_a_canonical_example_and_its_own_invalid_fixture(self):
        canonical = F.canonical_examples()
        invalid = [f.name for f in F.frame_fixtures("invalid")]
        for sender, frames in HP.FRAMES.items():
            for t in frames:
                self.assertIn((sender, t), canonical, f"no {sender}.{t}.json")
                if t not in ("ping", "pong", "rescan", "jobs_resume"):
                    self.assertTrue(any(n.startswith(f"{sender}.{t}.") for n in invalid),
                                    f"no invalid fixture {sender}.{t}.*.json")


class FrameTable(unittest.TestCase):
    def test_the_frame_table_is_exactly_the_types_this_side_knows(self):
        rows = F.frame_table(F.PROTOCOL_MD.read_text(encoding="utf-8"))
        self.assertEqual(len(rows), len(set(rows)), "a row appears twice in the frame table")
        known = {(t, sender) for sender, frames in HP.FRAMES.items() for t in frames}
        self.assertEqual(set(rows), known)

    def test_the_parser_reads_rows_and_refuses_a_broken_table(self):
        md = ("# x\n\n## The frame table\n\nText.\n\n| Frame | From | Purpose |\n|---|---|---|\n"
              "| `hello` | helper | the first |\n| `ping` | server | alive |\n\nAfter.\n| `x` | helper | no |\n")
        self.assertEqual(F.frame_table(md), [("hello", "helper"), ("ping", "server")])
        for broken in ("# no table", md.replace("| `ping` | server |", "| ping | server |"),
                       md.replace("| `ping` | server |", "| `ping` | hub |")):
            with self.assertRaises(ValueError):
                F.frame_table(broken)

    def test_the_v1_refusal_is_the_one_protocol_md_and_the_fixture_give(self):
        md = F.PROTOCOL_MD.read_text(encoding="utf-8")
        quoted = re.search(r'\{"t": "error", "ts": [0-9.]+, "id": "", "message": "([^"]+)"\}', md)
        self.assertIsNotNone(quoted)
        self.assertEqual(quoted.group(1), HP.V1_REFUSAL_MESSAGE)
        self.assertEqual(F.canonical_examples()[("server", "error")]["message"], HP.V1_REFUSAL_MESSAGE)
        frame = json.loads(HP.v1_refusal())
        self.assertEqual({k: v for k, v in frame.items() if k != "ts"},
                         {"t": "error", "id": "", "message": HP.V1_REFUSAL_MESSAGE})


class Schema(unittest.TestCase):
    def test_the_field_tables_are_the_committed_schema(self):
        """Every field table this side checks equals ``protocol/v2/schema.json``, which the helper's side is held to
        as well: a bound written differently on one side fails here even where no fixture reaches it."""
        committed = json.loads((F.FIXTURES / "schema.json").read_text(encoding="utf-8"))
        ours = json.loads(json.dumps(HP.schema()))
        self.assertEqual(set(ours), set(committed))
        for side, frames in committed.items():
            self.assertEqual(set(ours[side]), set(frames), side)
            for t, want in frames.items():
                with self.subTest(side=side, t=t):
                    self.assertTrue(F.json_eq(ours[side][t], want),
                                    f"{side} {t}:\n  this side {ours[side][t]}\n  committed {want}")


class Builders(unittest.TestCase):
    """The server's builders write frames its own parser, and the helper's, accept."""

    def test_each_server_frame_has_a_builder_that_writes_what_parses(self):
        ex = F.canonical_examples()
        welcome = ex[("server", "welcome")]
        built = {
            "welcome": HP.welcome(welcome["machine"]["id"], welcome["machine"]["name"], welcome["settings"], 20,
                                  welcome["server"], welcome["update"], [], [], []),
            "refused": HP.refused("rate", "slow down", retry_after_s=60),
            "error": HP.v1_refusal(),
            "settings": HP.settings(**{k: ex[("server", "settings")][k] for k in ("llm", "cpu")}),
            "rescan": HP.rescan(),
            "request": HP.request("0123456789abcdef", "m", [{"role": "user", "content": "hi"}], None, {}, 60),
            "cancel": HP.cancel("0123456789abcdef"),
            "job_offer": HP.job_offer("cj_1", "cr_1", "lab_game", {"exp": "x"}, "ab" * 32, "0123456789ab", 900, 256,
                                      0),
            "job_result_ack": HP.job_result_ack("cr_1"),
            "job_abort": HP.job_abort("cr_1", "expired"),
            "jobs_pause": HP.jobs_pause(),
            "jobs_resume": HP.jobs_resume(),
            "update": HP.update("{}", ex[("server", "update")]["signature"]),
            "ping": HP.ping(),
            "pong": HP.pong(),
            "shutdown": HP.shutdown("revoked", "The worker token was revoked."),
        }
        self.assertEqual(set(built), set(HP.SERVER_FRAMES))
        for t, text in built.items():
            with self.subTest(t):
                frame = HP.parse_server_frame(text)
                self.assertEqual(frame["t"], t)
                self.assertIsInstance(frame["ts"], float)
        self.assertNotIn("retry_after_s", json.loads(HP.refused("token", "no")))
        self.assertIsInstance(HP.parse_helper_frame(HP.ping(HP.HELPER)), dict)

    def test_the_close_reason_names_the_protocol_error_in_123_bytes(self):
        for raw, want in (('{"t": "hello", "protocol": 2}', "missing token"), ("[1]", "not_object"),
                          ("{", "not_json")):
            with self.assertRaises(HP.ProtocolError) as caught:
                HP.parse_helper_frame(raw)
            self.assertEqual(HP.close_reason(caught.exception), want)
        long = HP.ProtocolError("value", "llm.models." + "\u00e9" * 200, "x")
        reason = HP.close_reason(long)
        self.assertLessEqual(len(reason.encode("utf-8")), HP.MAX_CLOSE_REASON)
        self.assertTrue(reason.startswith("value llm.models."))
        reason.encode("utf-8").decode("utf-8")   # cut on a character, never inside one

    def test_seat_parameters_are_brought_within_the_protocol_s_bounds(self):
        """A seat's overrides are copied verbatim from what a member saved (``pool.seats``); the request builder refuses
        anything outside the protocol's bounds, so ``request_params`` coerces and clamps them first and says what it
        changed (4-05's ``WorkerConversation.step`` logs that once per seat)."""
        params, changed = HP.request_params({"temperature": 3, "max_tokens": "4096", "top_p": -0.5, "seed": -1,
                                             "reasoning_effort": "", "frequency_penalty": 0.5})
        self.assertEqual(params, {"temperature": 2.0, "max_tokens": 4096, "top_p": 0.0, "frequency_penalty": 0.5})
        self.assertEqual(changed, ["temperature", "max_tokens", "top_p", "seed", "reasoning_effort"])
        for raw, want in (({"max_tokens": 0}, {"max_tokens": 1}), ({"max_tokens": 4096.0}, {"max_tokens": 4096}),
                          ({"max_tokens": 10 ** 9}, {"max_tokens": 1048576}), ({"temperature": "0.7"},
                                                                                {"temperature": 0.7}),
                          ({"temperature": "hot"}, {}), ({"temperature": float("nan")}, {}),
                          ({"temperature": True}, {}), ({"max_tokens": 12.5}, {}), ({"seed": "7"}, {"seed": 7}),
                          ({"seed": 2 ** 60}, {}), ({"reasoning_effort": "x" * 33}, {}),
                          ({"reasoning_effort": "high"}, {"reasoning_effort": "high"}), ({"temperature": None}, {}),
                          ({"temperature": 0.7, "max_tokens": 2048}, {"temperature": 0.7, "max_tokens": 2048})):
            with self.subTest(raw=raw):
                self.assertEqual(HP.request_params(raw)[0], want)
                HP.request("0123456789abcdef", "m", [{"role": "user", "content": "hi"}], None,
                           HP.request_params(raw)[0], 60)
        self.assertEqual(HP.request_params({"temperature": 0.7, "max_tokens": 2048})[1], [])

    def test_a_builder_refuses_what_a_receiver_would(self):
        with self.assertRaises(HP.ProtocolError) as caught:
            HP.server_frame("cancel", id="has space")
        self.assertEqual((caught.exception.code, caught.exception.path), ("value", "id"))
        with self.assertRaises(HP.ProtocolError) as caught:
            HP.server_frame("cancel", id="0123456789abcdef", reason="typo")
        self.assertEqual((caught.exception.code, caught.exception.path), ("unknown", "reason"))
        with self.assertRaises(HP.ProtocolError) as caught:
            HP.server_frame("done", id="x")
        self.assertEqual(caught.exception.code, "no_type")
        with self.assertRaises(HP.ProtocolError):
            HP.shutdown("evicted", "bye")
        with self.assertRaises(HP.ProtocolError):
            HP.helper_frame("job_accept", run="run_1")

    def test_a_hello_that_is_not_protocol_2_is_refused_before_its_other_fields(self):
        for hello in ({"t": "hello"}, {"t": "hello", "protocol": 1, "token": 5},
                      {"t": "hello", "protocol": 2.0}, {"t": "hello", "protocol": True}):
            with self.assertRaises(HP.ProtocolError) as caught:
                HP.parse_helper_frame(json.dumps(hello))
            self.assertEqual(caught.exception.code, "protocol", hello)
        # A server's frames have no such check: a welcome for protocol 1 is an ordinary value error.
        with self.assertRaises(HP.ProtocolError) as caught:
            HP.parse_server_frame('{"t": "welcome", "protocol": 1}')
        self.assertEqual((caught.exception.code, caught.exception.path), ("value", "protocol"))

    def test_numbers_read_as_serde_json_reads_them(self):
        def code(text):
            try:
                HP.parse_helper_frame(text)
            except HP.ProtocolError as exc:
                return exc.code, exc.path
            return None

        status = '{"t": "status", "quiet_until": null, "llm_in_flight": %s, "jobs_running": 0, "jobs_paused": 0}'
        self.assertIsNone(code(status % "3"))
        self.assertEqual(code(status % "-0"), ("type", "llm_in_flight"))         # serde_json: a float
        self.assertEqual(code(status % "3.0"), ("type", "llm_in_flight"))
        self.assertEqual(code(status % "1e1"), ("type", "llm_in_flight"))
        self.assertEqual(code(status % "18446744073709551616"), ("type", "llm_in_flight"))   # 2^64: a float
        self.assertEqual(code(status % "18446744073709551615"), ("value", "llm_in_flight"))  # 2^64 - 1: an integer
        self.assertEqual(code(status % "-9223372036854775809"), ("type", "llm_in_flight"))
        self.assertEqual(code(status % ("9" * 400)), ("not_json", ""))            # beyond a float
        self.assertEqual(code(status % "Infinity"), ("not_json", ""))
        self.assertEqual(code(status % "true"), ("type", "llm_in_flight"))

    def test_the_caps_on_size_and_depth(self):
        ping = '{"t": "ping", "x": %s}'
        ok = ping % ("[" * 63 + "]" * 63)
        self.assertEqual(HP.parse_helper_frame(ok), {"t": "ping"})
        with self.assertRaises(HP.ProtocolError) as caught:
            HP.parse_helper_frame(ping % ("[" * 64 + "]" * 64))
        self.assertEqual(caught.exception.code, "too_deep")
        # Deep and not JSON is still too deep, as the helper's scan before parsing finds it.
        with self.assertRaises(HP.ProtocolError) as caught:
            HP.parse_helper_frame(ping % ("[" * 70))
        self.assertEqual(caught.exception.code, "too_deep")
        with self.assertRaises(HP.ProtocolError) as caught:
            HP.parse_helper_frame(ping % ("[" * 100000 + "]" * 100000))
        self.assertEqual(caught.exception.code, "too_deep")
        # Brackets inside strings do not count.
        self.assertEqual(HP.parse_helper_frame(ping % json.dumps("[" * 100)), {"t": "ping"})
        # Bytes, not characters: 2.1 million four-byte characters are over 8 MiB.
        big = '{"t": "ping", "x": "%s"}' % ("\U0001F600" * 2_100_000)
        with self.assertRaises(HP.ProtocolError) as caught:
            HP.parse_helper_frame(big)
        self.assertEqual(caught.exception.code, "too_large")


class Robustness(unittest.TestCase):
    """Gate 3: random mutations of the valid frames (keys dropped, types swapped, deep nesting, huge numbers,
    NaN-like strings, the text cut or spliced) raise nothing but ``ProtocolError``."""

    REPLACEMENTS = [None, True, 0, -1, 1.5, 2 ** 64 - 1, -(2 ** 63), 2 ** 53 + 1, 1e308, -1e308, "NaN", "Infinity",
                    "-Infinity", "1e400", "-0", "", "\u0000", "x" * 5000, [], {}, [[[[[[]]]]]], {"t": "hello"}]
    BAD_NUMBERS = ["NaN", "1e400", "-Infinity", "00", "1.", "-", "+1", "0x10", "1e", "--1", "9" * 400]

    @staticmethod
    def _nodes(v, here=()):
        out = [here]
        if isinstance(v, dict):
            for k, x in v.items():
                out.extend(Robustness._nodes(x, here + (k,)))
        elif isinstance(v, list):
            for i, x in enumerate(v):
                out.extend(Robustness._nodes(x, here + (i,)))
        return out

    @staticmethod
    def _set(root, path, value):
        if not path:
            return value
        cur = root
        for p in path[:-1]:
            cur = cur[p]
        cur[path[-1]] = value
        return root

    @staticmethod
    def _get(root, path):
        for p in path:
            root = root[p]
        return root

    def _mutate(self, rng: random.Random, frame) -> str:
        v = json.loads(json.dumps(frame))
        splices, cut = [], None
        for _ in range(rng.randint(1, 4)):
            path = rng.choice(self._nodes(v))
            node = self._get(v, path)
            op = rng.randrange(8)
            if op == 0 and isinstance(node, dict) and node:
                del node[rng.choice(list(node))]
            elif op in (1, 2):
                v = self._set(v, path, json.loads(json.dumps(rng.choice(self.REPLACEMENTS))))
            elif op == 3:
                deep = node
                for _ in range(rng.randrange(140)):
                    deep = [deep]
                v = self._set(v, path, deep)
            elif op == 4 and isinstance(node, dict):
                renamed = {(k.upper() if rng.random() < 0.5 else k + "_"): x for k, x in node.items()}
                v = self._set(v, path, renamed)
            elif op == 5:
                marker = f"@@{len(splices)}@@"
                v = self._set(v, path, marker)
                splices.append((marker, rng.choice(self.BAD_NUMBERS)))
            elif op == 6:
                cut = rng.random()
            elif op == 7 and isinstance(node, dict):
                node["t"] = rng.choice(self.REPLACEMENTS)
        text = json.dumps(v, ensure_ascii=False)
        for marker, bad in splices:
            text = text.replace(json.dumps(marker), bad)
        if cut is not None:
            text = text[: int(len(text) * cut)]
        return text

    def test_two_thousand_mutations_raise_nothing_but_protocol_error(self):
        frames = [f.frame for f in F.frame_fixtures("valid")]
        rng = random.Random(4_01)
        codes, accepted = set(), 0
        for _ in range(2000):
            text = self._mutate(rng, rng.choice(frames))
            for sender in (HP.HELPER, HP.SERVER):
                try:
                    got = HP.parse(text, sender)
                except HP.ProtocolError as exc:
                    self.assertIn(exc.code, HP.ERROR_CODES)
                    codes.add(exc.code)
                    continue
                accepted += 1
                if isinstance(got, dict):
                    json.dumps(got, allow_nan=False)   # what is accepted is JSON again
        for code in ("too_deep", "not_json", "no_type", "protocol", "missing", "type", "value"):
            self.assertIn(code, codes)
        self.assertGreater(accepted, 0)


class Transcripts(unittest.TestCase):
    """Gate 4, from the server's side."""

    def test_every_transcript_loads_and_its_frames_realize_into_valid_frames(self):
        all_ = F.transcripts()
        self.assertGreaterEqual(len(all_), 60)
        canonical = F.canonical_examples()
        golden = F.committed_golden()
        realized = 0
        for t in all_:
            bindings = {}
            for i, sender, pattern, invalid in t.frames():
                with self.subTest(transcript=t.name, step=i):
                    real = F.realize(pattern, bindings, t.values, golden,
                                     canonical.get((sender, pattern["t"])))
                    text = json.dumps(real)
                    if invalid:
                        with self.assertRaises(HP.ProtocolError) as caught:
                            HP.parse(text, sender)
                        self.assertEqual(caught.exception.code, invalid)
                    else:
                        back = HP.parse(text, sender)
                        self.assertTrue(F.json_eq(back, real),
                                        f"a field the protocol drops:\n  {real}\n  {back}")
                    realized += 1
        self.assertGreater(realized, 300)

    def test_every_golden_reference_resolves(self):
        """The ``<golden:SET/NAME[/FIELD]>`` values the transcripts name are the committed jobs set's (4-07)."""
        refs = set()

        def walk(v):
            if isinstance(v, str) and v.startswith("<golden:"):
                refs.add(v)
            elif isinstance(v, list):
                for x in v:
                    walk(x)
            elif isinstance(v, dict):
                for x in v.values():
                    walk(x)

        for p in (F.FIXTURES / "transcripts").glob("*.json"):
            walk(json.loads(p.read_text(encoding="utf-8")))
        self.assertGreaterEqual(len(refs), 10)
        golden = F.committed_golden()
        for r in sorted(refs):
            m = F.parse_matcher(r)
            self.assertEqual(m.kind, "golden", r)
            self.assertIsNotNone(golden(m.golden[0], m.golden[1:]), r)
        self.assertIsNone(golden("jobs", ("duel-z",)))
        self.assertIsNone(golden("other", ("duel-a",)))

    def test_the_transcripts_cover_the_protocol(self):
        names = {t.name for t in F.transcripts()}
        for want in ("handshake-welcome", "handshake-refused-token", "handshake-refused-key",
                     "handshake-refused-protocol", "handshake-refused-rate", "handshake-refused-build",
                     "handshake-v015-server", "handshake-v1-refusal-v015", "settings-capped", "settings-pushed",
                     "catalog-on-change", "hardware-rescan", "power-minute", "status-quiet-hours",
                     "completion-streamed-tool-call", "completion-not-streamed", "completion-busy",
                     "cancel-mid-stream", "cancel-after-done", "lost-connection-cancels", "job-offer-to-ack",
                     "job-reject-build", "job-abort", "job-deadline", "job-reconnect-keep", "job-reconnect-drop",
                     "job-reconnect-after-send", "job-duplicate-result", "job-lost-offer", "job-silent-run",
                     "job-reoffer-backoff", "jobs-pause", "update-notify", "keepalive-ping-pong", "shutdown-revoked"):
            self.assertIn(want, names)

    def test_the_loaders_vocabularies_are_protocol_md_s(self):
        md = F.PROTOCOL_MD.read_text(encoding="utf-8")

        def paragraph(start):
            at = md.index(start)
            end = md.find("\n\n", at)
            return md[at:] if end < 0 else md[at:end]

        first = paragraph("**Background frames.**").splitlines()[0].split(" come ")[0]
        self.assertEqual(tuple(first.split("`")[1::2]), F.BACKGROUND)
        for start, keys in (("**Setup keys**", F.HELPER_SETUP + F.HELPER_CONFIG + F.HELPER_HOST + F.HUB_SETUP
                             + F.HUB_MACHINE),
                            ("**Endpoint steps**", F.ENDPOINT_KEYS + F.RESPOND_KEYS),
                            ("**Host steps**", F.HOST_KEYS),
                            ("**Actions**", F.HELPER_ACTS + F.HUB_ACTS),
                            ("**State expectations**", F.HELPER_STATE + F.HUB_STATE)):
            text = paragraph(start)
            for key in keys:
                self.assertTrue(f"`{key}`" in text or f'"{key}"' in text, f"{start} does not name {key}")

    # The frames the server starts, and the hub acts that start them (PROTOCOL.md, "Actions").
    CAUSES = {"request": ("submit_request", "force_request"), "cancel": ("cancel_request",),
              "settings": ("set_settings",), "rescan": ("rescan",), "update": ("manifest_arrives",),
              "jobs_pause": ("benchmark_start",), "jobs_resume": ("benchmark_end",),
              "shutdown": ("revoke_token", "disable_machine", "second_connection"),
              "job_offer": ("queue_job", "force_offer"),
              "job_abort": ("abort_run", "expire_run", "delete_experiment")}

    @staticmethod
    def _cause(steps, i):
        """The step that caused step ``i`` on the hub's side: the nearest before it that is not an endpoint, host,
        state or wait step (none of which the hub acts on)."""
        for step in reversed(steps[:i]):
            if step.action not in ("endpoint", "host", "state", "wait"):
                return step
        return None

    @staticmethod
    def _after_welcome(steps, i) -> bool:
        """Whether step ``i`` comes right after a welcome and its acknowledgement (nothing the hub acts on between)."""
        seen = []
        for step in reversed(steps[:i]):
            if step.action in ("endpoint", "host", "state", "wait"):
                continue
            seen.append(step)
            if len(seen) == 2:
                break
        kinds = [(st.action, st.body.get("t")) for st in seen]
        return kinds[:1] == [("s2h", "welcome")] or kinds == [("h2s", "settings_ack"), ("s2h", "welcome")]

    def test_every_frame_the_hub_starts_has_its_act(self):
        """The hub's side can drive every transcript it plays: a frame the server starts (a request, a cancel, a job
        offer, a pushed setting...) and a connection the server drops are each caused by a hub act just before them.
        A later offer of a job already offered is the dispatcher's own re-offer, a job_abort may answer a late
        job_accept, one right after a wait is the dispatcher's own expiry of a run (PROTOCOL.md: "An expired run
        is aborted"), and a jobs_pause right after a welcome says again a pause whose benchmark lasts (PROTOCOL.md: "A
        connection's own")."""
        checked = 0
        for t in F.transcripts():
            if t.plays["hub"] is None:
                continue
            offered = set()
            for i, step in enumerate(t.steps):
                with self.subTest(transcript=t.name, step=i):
                    cause = self._cause(t.steps, i)
                    word = F.act_word(cause.body["do"]) if cause is not None and cause.action == "act" else None
                    if step.action == "drop" and step.body["by"] == HP.SERVER:
                        self.assertEqual(word, "drop_connection")
                        checked += 1
                    if step.action != "s2h" or step.body["t"] not in self.CAUSES:
                        continue
                    t_ = step.body["t"]
                    if t_ == "job_offer":
                        job = step.body["job"]
                        if job in offered:
                            continue
                        offered.add(job)
                    if t_ == "job_abort" and cause is not None and cause.action == "h2s" \
                            and cause.body["t"] == "job_accept":
                        continue
                    if t_ == "job_abort" and t.steps[i - 1].action == "wait":
                        continue
                    if t_ == "jobs_pause" and self._after_welcome(t.steps, i):
                        continue
                    self.assertIn(word, self.CAUSES[t_], f"{t_} needs one of {self.CAUSES[t_]} before it")
                    checked += 1
        self.assertGreater(checked, 50)

    @staticmethod
    def _literal(v) -> bool:
        if isinstance(v, dict):
            return all(Transcripts._literal(x) for x in v.values())
        if isinstance(v, list):
            return all(Transcripts._literal(x) for x in v)
        return F.parse_matcher(v) is None

    def test_every_acknowledgement_is_what_a_correct_helper_sends(self):
        """Every transcript that reaches a welcome states the site's settings (the hub's setup and the welcome, the
        same object) and the helper's slots and CPU ceiling, and each settings_ack the helper plays is what those make
        by PROTOCOL.md's rules, written out in full, so the hub side's fake helper acknowledges exactly that."""
        acks = 0
        for t in F.transcripts():
            site = None
            for i, step in enumerate(t.steps):
                with self.subTest(transcript=t.name, step=i):
                    if step.action == "s2h" and step.body["t"] == "welcome":
                        site = step.body["settings"]
                        self.assertIsInstance(site, dict, "the welcome's settings are written out")
                        self.assertTrue(self._literal(site))
                        if t.plays["hub"] is not None:
                            self.assertEqual(site, t.setup["hub"]["machine"]["settings"])
                    elif step.action == "s2h" and step.body["t"] == "settings":
                        site = {k: step.body[k] for k in ("llm", "cpu")}
                    elif step.action == "h2s" and step.body["t"] == "settings_ack":
                        self.assertIsNotNone(site, "an acknowledgement of no settings")
                        self.assertTrue(self._literal({k: step.body[k] for k in ("effective", "capped")}))
                        if t.plays["helper"] is not None:
                            config = t.setup["helper"]["config"]
                            self.assertIn("cpu", config)
                            want = F.expected_ack(site, config)
                            self.assertEqual({k: step.body[k] for k in ("effective", "capped")}, want)
                        acks += 1
        self.assertGreater(acks, 60)

    def test_the_acknowledgement_rules(self):
        site = {"llm": {"enabled": True, "slots": 4, "allow_swaps": False},
                "cpu": {"enabled": True, "threads": 0, "cpu_percent": 25, "memory_mb": 4096,
                        "work": ["reference_game", "lab_game"], "yield_to_llm": False}}
        config = {"max_concurrent": 2, "cpu": {"enabled": False, "threads": 6, "cpu_percent": 50, "memory_mb": 1024,
                                               "work": ["lab_game", "ranking_game"]}}
        self.assertEqual(F.expected_ack(site, config), {
            "effective": {"llm": {"enabled": True, "slots": 2},
                          "cpu": {"enabled": False, "threads": 6, "cpu_percent": 25, "memory_mb": 1024,
                                  "work": ["lab_game"], "yield_to_llm": False}},
            "capped": ["llm.slots", "cpu.enabled", "cpu.memory_mb", "cpu.work"]})
        no_llm = {**config, "no_llm": True}
        self.assertEqual(F.expected_ack(site, no_llm)["effective"]["llm"], {"enabled": False, "slots": 0})
        self.assertEqual(F.expected_ack(site, no_llm)["capped"][:2], ["llm.enabled", "llm.slots"])

    def test_the_hub_plays_the_v1_refusal_and_the_helper_never_does(self):
        for t in F.transcripts():
            if t.name.startswith("handshake-v1-refusal-") and t.name != "handshake-v1-refusal-received":
                self.assertEqual(t.plays, {"helper": None, "hub": "4-05"}, t.name)
                self.assertEqual(t.steps[0].invalid, "protocol", t.name)
                self.assertEqual(t.steps[1].body["message"], HP.V1_REFUSAL_MESSAGE)
                self.assertEqual(t.steps[2].body, {"by": "server", "code": HP.CLOSE_PROTOCOL})

    def _base(self):
        return {"name": "t", "about": "a test", "refs": ["P4.2.2"], "plays": {"helper": "4-03", "hub": "4-05"},
                "values": {"req": "0123456789abcdef"}, "setup": {"helper": {}, "hub": {}},
                "steps": [{"s2h": {"t": "cancel", "id": "<id:req>"}}]}

    def _with(self, path, value):
        t = self._base()
        cur = t
        for p in path[:-1]:
            cur = cur[p]
        cur[path[-1]] = value
        return t

    def test_a_good_transcript_loads_and_each_defect_is_refused(self):
        self.assertEqual(len(F.transcript_from(self._base()).steps), 1)
        for bad, why in (
            (self._with(["values"], {}), "no example"),
            (self._with(["plays", "hub"], None), "one_side must say why"),
            (self._with(["plays", "hub"], "5-01"), "a package"),
            (self._with(["steps"], [{"s2h": {"t": "hello"}}]), "never sends"),
            (self._with(["steps"], [{"h2s": {"t": "welcome"}}]), "never sends"),
            (self._with(["steps"], [{"s2h": {"t": "ping"}, "h2s": {"t": "pong"}}]), "exactly one"),
            (self._with(["steps"], [{"wait": {"s": 0}}]), "positive"),
            (self._with(["steps"], [{"connect": {}, "invalid": "missing"}]), "only a frame"),
            (self._with(["steps"], [{"s2h": {"t": "ping"}, "invalid": "broken"}]), "error code"),
            (self._with(["steps"], [{"s2h": {"t": "ping", "x": "<int:a..>"}}]), "is not a bound"),
            (self._with(["steps"], [{"s2h": {"t": "ping", "x": "<int:5>"}}]), "LO..HI"),
            (self._with(["steps"], [{"endpoint": {"respond": {"jsn": {}}}}]), "unknown key"),
            (self._with(["steps"], [{"state": {"side": "server", "expect": {}}}]), '"hub"'),
            (self._with(["setup", "helper"], {"confg": {}}), "unknown key"),
            (self._with(["setup", "hub"], {"machine": {"settngs": {}}}), "unknown key"),
            (self._with(["setup", "helper"], {"host": {"canary": False}}), "unknown key"),
            (self._with(["extra"], 1), "unknown key"),
            (self._with(["steps"], [{"act": {"side": "hub", "do": "sumbit_request"}}]), "not a word"),
            (self._with(["steps"], [{"act": {"side": "helper", "do": "rescan"}}]), "not a word"),
            (self._with(["steps"], [{"act": {"side": "hub", "do": {"a": 1, "b": 2}}}]), "one word"),
            (self._with(["steps"], [{"act": {"side": "hub", "do": 5}}]), "one word"),
            (self._with(["steps"], [{"state": {"side": "hub", "expect": {"slots": 1}}}]), "unknown key"),
            (self._with(["steps"], [{"state": {"side": "helper", "expect": {"draining": 0}}}]), "unknown key"),
            (self._with(["steps"], [{"connect": {}, "lost": True}]), "frame step only"),
            (self._with(["steps"], [{"s2h": {"t": "ping"}, "lost": False}]), "frame step only"),
            (self._with(["steps"], [{"close": {"by": "server", "code": 4400, "reason": 1}}]), "123 bytes"),
            (self._with(["steps"], [{"close": {"by": "server", "code": 4400, "reason": "x" * 124}}]), "123 bytes"),
        ):
            with self.subTest(why):
                with self.assertRaises(ValueError) as caught:
                    F.transcript_from(bad)
                self.assertIn(why, str(caught.exception))
        one = self._with(["plays", "hub"], None)
        one["one_side"] = "scripts a server"
        F.transcript_from(one)
        both = self._base()
        both["one_side"] = "but both play"
        with self.assertRaises(ValueError):
            F.transcript_from(both)

    def test_acts_states_closes_and_lost_frames_load(self):
        t = F.transcript_from(self._with(["steps"], [
            {"act": {"side": "hub", "do": "submit_request"}},
            {"act": {"side": "hub", "do": {"set_settings": {}}}},
            {"act": {"side": "helper", "do": "quiet_begins"}},
            {"s2h": {"t": "ping"}, "lost": True},
            {"close": {"by": "server", "code": 4400, "reason": "missing machine"}},
            {"state": {"side": "hub", "expect": {"slot_free": True, "draining": 0}}},
            {"state": {"side": "helper", "expect": {"in_flight": 0}}}]))
        self.assertEqual([s.lost for s in t.steps], [False, False, False, True, False, False, False])
        self.assertEqual(t.steps[4].body["reason"], "missing machine")
        self.assertEqual(F.act_word(t.steps[1].body["do"]), "set_settings")


class Matchers(unittest.TestCase):
    """Gate 4: each matcher accepts what it stands for and refuses the rest, as the helper's copy does."""

    @staticmethod
    def golden(set_name, path):
        return 30 if (set_name, path) == ("jobs", ("duel-a", "rounds")) else None

    def check(self, pattern, actual):
        F.matches(pattern, actual, {}, self.golden)

    def test_each_matcher_accepts_and_refuses(self):
        cases = [
            ("<any>", [None, 1, "x", {}], []),
            ("<bool>", [True, False], [0, "true", None]),
            ("<int>", [0, -5, 2 ** 64 - 1], [1.5, 2.0, True, "1"]),
            ("<int:1..>", [1, 99], [0, -1]),
            ("<int:-100..100>", [-100, 100], [101, -101]),
            ("<int:..3>", [3, -9], [4]),
            ("<number>", [0, 1.5, -2], ["1", None]),
            ("<number:0..1>", [0, 0.5, 1], [1.01, -0.1]),
            ("<number:0.001..>", [0.001, 9], [0]),
            ("<string>", ["", "x"], [1, None]),
            ("<hex12>", ["0123456789ab"], ["0123456789AB", "0123456789a", 1]),
            ("<hex16>", ["0123456789abcdef"], ["0123456789abcde"]),
            ("<hex64>", ["ab" * 32], ["ab" * 31, "zz" * 32]),
            ("<base64>", ["AAEC", "AA=="], ["", "AA=", "AB==", "**"]),
            ("<golden:jobs/duel-a/rounds>", [30, 30.0], [31]),
            ("<golden:jobs/duel-b/rounds>", [], [30]),
        ]
        for pattern, yes, no in cases:
            for v in yes:
                with self.subTest(pattern=pattern, accepts=v):
                    self.check(pattern, v)
            for v in no:
                with self.subTest(pattern=pattern, refuses=v):
                    with self.assertRaises(F.Mismatch):
                        self.check(pattern, v)

    def test_ids_bind_once_and_hold_across_frames(self):
        b = {}
        F.matches({"id": "<id:req>"}, {"id": "abc"}, b)
        self.assertEqual(b, {"req": "abc"})
        F.matches({"id": "<id:req>"}, {"id": "abc", "x": 1}, b)
        with self.assertRaises(F.Mismatch) as caught:
            F.matches({"id": "<id:req>"}, {"id": "abd"}, b)
        self.assertEqual(caught.exception.path, "id")
        fresh = {}
        with self.assertRaises(F.Mismatch):
            F.matches(["<id:a>", 2], [1, 3], fresh)
        self.assertEqual(fresh, {})
        with self.assertRaises(F.Mismatch):
            F.matches("<id:a>", None, fresh)

    def test_objects_are_open_and_arrays_closed(self):
        self.check({"a": 1}, {"a": 1, "b": 2})
        self.check({"a": 1}, {"a": 1.0})
        with self.assertRaises(F.Mismatch) as caught:
            self.check({"a": {"b": "<int>"}}, {"a": {}})
        self.assertEqual((caught.exception.path, caught.exception.why), ("a.b", "absent"))
        for pattern, actual in (([1, 2], [1, 2, 3]), ([1, "<any>"], [1]), (None, 0), (True, 1)):
            with self.assertRaises(F.Mismatch):
                self.check(pattern, actual)
        with self.assertRaises(F.Mismatch) as caught:
            self.check({"xs": [1, "<string>"]}, {"xs": [1, 2]})
        self.assertEqual(caught.exception.path, "xs.1")

    def test_strings_that_are_not_matchers_are_literals_and_bad_matchers_are_errors(self):
        for literal in ("<think>", "<think>hi</think>", "plain"):
            self.assertIsNone(F.parse_matcher(literal))
        self.check("<think>", "<think>")
        for bad in ("<int:a..b>", "<int:5>", "<any:x>", "<id:>", "<id:Up>", "<golden:jobs>", "<hex12:3>"):
            with self.assertRaises(F.MatcherError):
                F.parse_matcher(bad)
            with self.assertRaises(F.Mismatch):
                self.check(bad, 1)
        self.assertEqual(F.parse_matcher("<int:1..>"), F.Matcher("int", lo=1))
        self.assertEqual(F.parse_matcher("<golden:jobs/duel-a/spec_hash>").golden,
                         ("jobs", "duel-a", "spec_hash"))

    def test_realizing_takes_bindings_values_examples_then_defaults(self):
        values = {"req": "0123456789abcdef", "n": 7}
        b = {"token": "tok"}
        pattern = {"t": "x", "token": "<id:token>", "id": "<id:req>", "count": "<int:1..>", "rate": "<number:0.5..>",
                   "name": "<string>", "list": ["<hex12>", "<hex12>"], "flag": "<bool>", "key": "<base64>",
                   "g": "<golden:jobs/duel-a/rounds>", "h": "<golden:jobs/duel-b/rounds>", "any": "<any>"}
        example = {"count": 0, "rate": 2.0, "name": "ex", "list": ["aaaaaaaaaaaa"], "h": 12, "any": {"k": 1}}
        out = F.realize(pattern, b, values, self.golden, example)
        self.assertEqual(out, {"t": "x", "token": "tok", "id": "0123456789abcdef", "count": 1, "rate": 2.0,
                               "name": "ex", "list": ["aaaaaaaaaaaa", "aaaaaaaaaaaa"], "flag": False,
                               "key": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=", "g": 30, "h": 12,
                               "any": {"k": 1}})
        self.assertEqual(b["req"], "0123456789abcdef")
        with self.assertRaises(ValueError):
            F.realize("<id:nobody>", b, values)
        with self.assertRaises(ValueError):
            F.realize("<golden:jobs/x>", b, values)
        with self.assertRaises(F.Mismatch) as caught:
            F.matches(pattern, out, dict(b), self.golden)
        self.assertEqual(caught.exception.path, "h")


if __name__ == "__main__":
    unittest.main()
