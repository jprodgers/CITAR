"""The server soak: one lobby game of bots and a model seat, played to its end on the dev server, through a restart.

Package 2-13's run of crates/citar-engine/DESIGN.md P2.1.3 ("the server runs on Rust"): the server on the Rust engine
plays a whole game the way people use it, with the side effects a turn has, and survives being killed.

    python scripts/server_soak.py STATE_DIR [--port N] [--size standard] [--bots 8] [--seed S]
                                  [--chat-turn T] [--restart-turn T] [--json FILE]

It starts ``scripts/dev_server.py`` (local mode, debug helpers on, everything under STATE_DIR, which it empties first)
and creates a lobby game (``POST /api/games``) of a model seat and N bots, the model seat first. Then:

- **the broadcasts**: a spectator's socket (``/ws/games/<id>``) keeps every message; every bot turn the game's metrics
  record must have been announced by a ``turn`` broadcast;
- **the model seat** plays through the MCP bridge (``citar.agents.mcp_server`` over stdio, an MCP client as a model's
  host is): it founds its capital, answers every chat a bot opens with it, and ends its turns. At the chat turn it
  meets everyone (the debug helper) and opens a chat with a bot, which must answer it;
- **the restart**: right after the model seat ends the restart turn, with the bots in the middle of their round, the
  server is killed outright (no shutdown) and started again on the same directory. ``restore_live`` must bring the
  game back from its autosave, at most one round behind, and running; the bridge and the socket reconnect;
- **the end**: the game plays to its end (a victory, or the turn limit), and its replay loads;
- **the save lock**: how long the session's saves held its lock, from ``/debug/errors`` (``save_lock``), before the
  restart and at the end; P2.1.3's budget is 10 ms.

Prints a line per step and a JSON summary, which ``--json`` also writes; exits 0 when every check passed, 1 when one
failed, 2 when the server would not start. Never use the port or the directory of a server someone is playing on.
It needs the extension (``cargo xtask develop``) and the ``mcp`` and ``worker`` extras (the MCP client, and the
websockets the socket uses), which the ``dev`` extra includes.
"""
from __future__ import annotations

import argparse
import json
import os
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Optional

import anyio
import httpx
from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client
from websockets.exceptions import ConnectionClosed
from websockets.sync.client import connect

ROOT = Path(__file__).resolve().parent.parent
SAVE_LOCK_BUDGET_S = 0.010


def free_port() -> int:
    """A port nothing listens on now."""
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Report:
    """The steps and their outcomes, printed as they come and kept for the summary."""

    def __init__(self):
        self.started = time.perf_counter()
        self.steps: list[dict] = []
        self.data: dict = {}

    def step(self, name: str, ok: bool, detail: str = "") -> bool:
        """Record one check; returns whether it passed."""
        t = time.perf_counter() - self.started
        print(f"[{t:7.1f} s] [{'ok' if ok else 'FAIL'}] {name}{': ' + detail if detail else ''}", flush=True)
        self.steps.append({"step": name, "ok": ok, "at_s": round(t, 2), "detail": detail})
        return ok

    def note(self, text: str):
        """Print a line that is no check."""
        print(f"[{time.perf_counter() - self.started:7.1f} s] {text}", flush=True)


class Server:
    """The dev server as a child process, on one port and one state directory, which it can kill and start again."""

    def __init__(self, state: Path, port: int, report: Report):
        self.state, self.port, self.report = state, port, report
        self.base = f"http://127.0.0.1:{port}"
        self.proc: Optional[subprocess.Popen] = None
        self.logs: list[Path] = []

    def start(self, reset: bool) -> httpx.Client:
        """Start the server (emptying its directory first when ``reset``) and return a client logged in to it."""
        log = self.state.parent / f"{self.state.name}-server-{len(self.logs) + 1}.log"
        self.logs.append(log)
        args = [sys.executable, str(ROOT / "scripts" / "dev_server.py"), "--port", str(self.port),
                "--dir", str(self.state)] + (["--reset"] if reset else [])
        out = open(log, "w", encoding="utf-8")
        self.proc = subprocess.Popen(args, cwd=str(ROOT), stdout=out, stderr=subprocess.STDOUT,
                                     env={**os.environ, "PYTHONUNBUFFERED": "1"})
        out.close()
        client = httpx.Client(base_url=self.base, timeout=120.0)
        t0 = time.perf_counter()
        while True:
            try:
                if client.get("/api/status").status_code == 200:
                    break
            except httpx.HTTPError:
                pass
            if self.proc.poll() is not None or time.perf_counter() - t0 > 120:
                raise SystemExit(f"the server did not start (exit {self.proc.poll()}); see {log}")
            time.sleep(0.25)
        me = client.get("/api/auth/me").json()
        client.headers["x-citar-csrf"] = me.get("csrf_token", "")
        return client

    def kill(self):
        """Kill the server outright, as a power cut or a killed process would: no shutdown, no last save."""
        if self.proc is not None and self.proc.poll() is None:
            self.proc.kill()
            self.proc.wait(timeout=30)

    def stop(self):
        """Stop the server at the end."""
        if self.proc is not None and self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=30)
            except subprocess.TimeoutExpired:
                self.proc.kill()

    def tracebacks(self) -> int:
        """Tracebacks in the server's logs, every run's."""
        return sum(p.read_text(encoding="utf-8", errors="replace").count("Traceback") for p in self.logs if p.exists())


class Watcher(threading.Thread):
    """A spectator's socket: keeps every message the game broadcasts, with when it came."""

    def __init__(self, url: str):
        super().__init__(daemon=True)
        self.url = url
        self.messages: list[tuple[float, dict]] = []
        self.ready = threading.Event()
        self.done = threading.Event()

    def run(self):
        """Read until the socket closes or the soak says stop."""
        try:
            with connect(self.url, open_timeout=30, max_size=None) as ws:
                self.ready.set()
                while not self.done.is_set():
                    try:
                        raw = ws.recv(timeout=0.5)
                    except TimeoutError:
                        continue
                    except ConnectionClosed:
                        break
                    self.messages.append((time.time(), json.loads(raw)))
        finally:
            self.ready.set()

    def turns(self) -> list[tuple[int, int, str]]:
        """Every ``turn`` broadcast: (turn, current player, phase)."""
        return [(m["turn"], m["current_player"], m["phase"]) for _, m in self.messages if m.get("type") == "turn"]

    def of_type(self, kind: str) -> list[dict]:
        """Every message of one type."""
        return [m for _, m in self.messages if m.get("type") == kind]


class Soak:
    """The run: the server, the game, the socket and the model seat."""

    def __init__(self, args, report: Report):
        self.args, self.report = args, report
        self.server = Server(Path(args.state).resolve(), args.port or free_port(), report)
        self.client: Optional[httpx.Client] = None
        self.gid = ""
        self.model_token = ""
        self.spectator = ""
        self.watchers: list[Watcher] = []
        self.restarted = False
        self.killed_at: tuple[int, int, str] = (0, 0, "playing")
        self.unwatched_from: Optional[int] = None
        self.chat: dict = {}
        self.model = {"turns": 0, "answered": 0, "calls": 0, "status": None}
        self.save_lock: list[dict] = []

    # ------------------------------------------------------------------ the server and the socket
    def watch(self):
        """Open a spectator's socket on the game, and wait until it is connected."""
        w = Watcher(f"ws://127.0.0.1:{self.server.port}/ws/games/{self.gid}?token={self.spectator}")
        w.start()
        w.ready.wait(30)
        self.watchers.append(w)

    def game(self) -> dict:
        """The game as the lobby lists it."""
        return self.client.get(f"/api/games/{self.gid}").json()

    def lock_times(self, when: str):
        """The session's save lock counters, from the debug errors route."""
        r = self.client.get(f"/api/games/{self.gid}/debug/errors")
        body = r.json() if r.status_code == 200 else {}
        lock = dict(body.get("save_lock") or {})
        lock["when"] = when
        lock["run"] = len(self.server.logs)          # which start of the server: the counts start again at each
        lock["errors"] = len(body.get("errors") or [])
        lock["writer"] = body.get("save_writer")
        self.save_lock.append(lock)
        self.report.note(f"saves {when}: {json.dumps(lock)}")
        return lock

    def kill_mid_round(self, turn: int):
        """Kill the server once the bots are a few turns into the round after the model seat's ``turn``: the game
        has moved on from its last autosave (taken as the round began), and a writer may be part way through one."""
        w = self.watchers[-1]
        deadline = time.perf_counter() + 2.0
        while time.perf_counter() < deadline:
            seen = w.turns()
            if seen and (seen[-1][0] > turn or seen[-1][1] >= 3):
                break
            time.sleep(0.002)
        at_kill = self.lock_times("as the server is killed")
        self.server.kill()
        w.done.set()
        self.report.data["writer_at_kill"] = at_kill.get("writer")
        seen = w.turns()
        self.killed_at = seen[-1] if seen else (turn, 0, "playing")
        self.report.note(f"killed the server at turn {self.killed_at[0]}, player {self.killed_at[1]} to move "
                         f"(the last broadcast)")

    def restart_without_the_model(self):
        """The restart, when the model seat left the game before the restart turn: once the bots reach that turn,
        kill the server as they play. restore_live then resumes the game as the server starts, before the socket is
        back, so the bot turns of that gap are not watched; ``check_broadcasts`` leaves them out and says how many."""
        w = self.watchers[-1]
        while (seen := w.turns()) and seen[-1][0] < self.args.restart_turn and seen[-1][2] == "playing":
            time.sleep(0.002)
        self.kill_mid_round(seen[-1][0] if seen else 0)
        self.unwatched_from = self.killed_at[0]
        self.restart()

    def restart(self):
        """Start the killed server again on its directory, and check what restore_live brought back."""
        last = self.killed_at
        self.client.close()
        self.client = self.server.start(reset=False)
        after = self.game()
        behind = last[0] - after["turn"]
        self.report.data["restart"] = {"killed_at": {"turn": last[0], "current": last[1]},
                                       "restored": {"turn": after["turn"], "current": after["current_player"],
                                                    "paused": after["paused"], "phase": after["phase"]},
                                       "rounds_behind": behind}
        self.report.step("restore_live brought the game back, running", after["phase"] == "playing"
                         and not after["paused"], f"turn {after['turn']}, player {after['current_player']} to move")
        self.report.step("its autosave at most one round behind", 0 <= behind <= 1,
                         f"killed at turn {last[0]}, restored at turn {after['turn']}: {behind} round(s) behind")
        self.watch()
        self.restarted = True

    # ------------------------------------------------------------------ the model seat
    async def model_seat(self):
        """The model seat through the MCP bridge, again after each restart, until its game ends for it."""
        while True:
            outcome = await self.bridge_session()
            self.model["status"] = outcome
            if outcome != "restart":
                return outcome
            self.restart()

    async def bridge_session(self) -> str:
        """One bridge process and its MCP session: play until the game is over for the seat, or a restart is due."""
        params = StdioServerParameters(
            command=sys.executable,
            args=["-m", "citar.agents.mcp_server", "--server", self.server.base, "--game", self.gid,
                  "--token", self.model_token],
            cwd=str(ROOT), env=dict(os.environ))
        async with stdio_client(params) as (read, write):
            async with ClientSession(read, write) as session:
                await session.initialize()
                tools = {t.name for t in (await session.list_tools()).tools}
                missing = {"wait_for_turn", "end_turn", "open_negotiation", "respond_negotiation"} - tools
                if missing:
                    self.report.step("the bridge lists the seat's tools", False, f"missing {sorted(missing)}")
                    return "broken"
                return await self.play(session)

    async def call(self, session: ClientSession, name: str, args: Optional[dict] = None):
        """Call one tool through the bridge: (ok, the result parsed as JSON where it is JSON)."""
        self.model["calls"] += 1
        res = await session.call_tool(name, args or {})
        text = "".join(getattr(c, "text", "") for c in res.content)
        try:
            value = json.loads(text)
        except ValueError:
            value = text
        return not res.is_error, value

    async def answer_chats(self, session: ClientSession, nids: list[int]):
        """Answer each chat that waits on the model seat: no, politely."""
        for nid in nids:
            ok, _ = await self.call(session, "respond_negotiation",
                                    {"negotiation_id": nid, "action": "reject", "message": "Not now, thank you."})
            self.model["answered"] += int(ok)

    async def found_capital(self, session: ClientSession):
        """Found the capital with the first settler."""
        ok, units = await self.call(session, "get_units")
        listed = units.get("units", []) if isinstance(units, dict) else units if isinstance(units, list) else []
        settler = next((u for u in listed if "settler" in str(u.get("type") or u.get("name") or "").lower()), None)
        if settler:
            ok, res = await self.call(session, "found_city", {"unit_id": settler["id"]})
            self.report.step("the model seat founds its capital", ok, str(res)[:120])

    async def open_chat(self, session: ClientSession, turn: int):
        """Meet everyone, open a chat with the first bot still alive, and see that it answers."""
        self.client.post(f"/api/games/{self.gid}/debug/meet_all")
        info = self.game()
        bots = [s["player"] for s in info["seats"] if s["type"] == "bot"]
        alive = {p["id"] for p in info["players"] if p["alive"]}
        to = next((p for p in bots if p in alive), None)
        if to is None:
            self.report.step("a chat with a bot", False, "no bot is alive")
            return
        t0 = time.perf_counter()
        ok, res = await self.call(session, "open_negotiation", {
            "to": to, "message": "Greetings from the soak. A gift of gold, for friendship's sake?",
            "give": [{"type": "gold", "amount": 1}]})
        nid = res.get("negotiation_id") or res.get("id") if ok and isinstance(res, dict) else None
        found = None
        for _ in range(4):
            _, dip = await self.call(session, "get_diplomacy")
            found = _find_negotiation(dip, nid)
            if found and (found.get("status") != "open" or found.get("awaiting") != to):
                break
            await self.call(session, "wait_for_turn", {"timeout_seconds": 15})
        took = time.perf_counter() - t0
        answered = bool(found) and (found.get("status") != "open" or found.get("awaiting") != to)
        self.chat = {"turn": turn, "to": to, "opened": ok, "negotiation": found, "seconds": round(took, 2)}
        self.report.step(f"a chat with bot {to} at turn {turn}, answered", ok and answered,
                         f"{json.dumps(found)[:300]} in {took:.2f} s")
        if found and found.get("status") == "open":
            await self.call(session, "respond_negotiation",
                            {"negotiation_id": nid, "action": "reject", "message": "Thank you, another time."})

    async def end_turn(self, session: ClientSession) -> bool:
        """End the model seat's turn, answering whatever chat holds it first."""
        for _ in range(5):
            ok, res = await self.call(session, "end_turn")
            if ok:
                return True
            w_ok, w = await self.call(session, "wait_for_turn", {"timeout_seconds": 5})
            if w_ok and isinstance(w, dict) and w.get("status") == "negotiation":
                await self.answer_chats(session, w.get("negotiation_ids", []))
        self.report.step("the model seat ends its turn", False, str(res)[:200])
        return False

    async def play(self, session: ClientSession) -> str:
        """The model seat's loop: wait, answer, act, end the turn."""
        while True:
            ok, w = await self.call(session, "wait_for_turn", {"timeout_seconds": 50})
            if not ok or not isinstance(w, dict):
                self.report.step("wait_for_turn", False, str(w)[:200])
                return "broken"
            status = w.get("status")
            if status in ("game_over", "eliminated", "crashed"):
                return status
            if status == "negotiation":
                await self.answer_chats(session, w.get("negotiation_ids", []))
                continue
            if status != "your_turn":
                continue
            turn = w["turn"]
            self.model["turns"] += 1
            if self.model["turns"] == 1:
                await self.found_capital(session)
            if turn >= self.args.chat_turn and not self.chat:
                await self.open_chat(session, turn)
            restart = turn >= self.args.restart_turn and not self.restarted
            if restart:
                self.lock_times("before the restart")
            if not await self.end_turn(session):
                return "broken"
            if restart:
                self.kill_mid_round(turn)
                return "restart"

    # ------------------------------------------------------------------ the run
    def run(self) -> int:
        """Play the game, check it, and report."""
        r = self.report
        state = self.server.state
        state.parent.mkdir(parents=True, exist_ok=True)
        try:
            self.client = self.server.start(reset=True)
            meta = self.client.get("/api/meta").json()
            r.data["server"] = {"port": self.server.port, "state": str(state), "engine": meta.get("engine")}
            seats = [{"type": "mcp", "name": "Soak model"}] + [{"type": "bot"}] * self.args.bots
            created = self.client.post("/api/games", json={
                "name": "server soak", "config": {"map_size": self.args.size, "seed": self.args.seed},
                "seats": seats})
            if not r.step("create the lobby game", created.status_code == 200, created.text[:200]):
                return 1
            info = created.json()
            self.gid, self.spectator = info["id"], info["spectator_token"]
            self.model_token = info["seats"][0]["token"]
            self.client.post(f"/api/games/{self.gid}/control", json={"ai_delay": 0})
            r.data["game"] = {"id": self.gid, "config": info["config"], "seats": len(info["seats"])}
            self.watch()
            t0 = time.perf_counter()
            outcome = anyio.run(self.model_seat)
            r.note(f"the model seat's game ended for it: {outcome}, after {self.model['turns']} turns")
            if not self.restarted and self.game()["phase"] == "playing":
                self.restart_without_the_model()
            deadline = time.perf_counter() + self.args.timeout
            while (g := self.game())["phase"] == "playing" and time.perf_counter() < deadline:
                time.sleep(1.0)
            played = time.perf_counter() - t0
            r.data["played_seconds"] = round(played, 1)
            self.lock_times("at the end")
            self.check_end(g)
            self.check_broadcasts()
            self.check_replay()
        finally:
            for w in self.watchers:
                w.done.set()
            if self.client is not None:
                self.client.close()
            self.server.stop()
        tracebacks = self.server.tracebacks()
        r.step("no traceback in the server's logs", tracebacks == 0, f"{tracebacks} in {len(self.server.logs)} logs")
        r.data["model_seat"] = self.model
        r.data["chat"] = self.chat
        r.data["save_lock"] = self.save_lock
        r.data["logs"] = [str(p) for p in self.server.logs]
        return 0 if all(s["ok"] for s in r.steps) else 1

    def check_end(self, g: dict):
        """The game ended, by a victory or its turn limit, without a crash or an agent's error."""
        r = self.report
        errors = [m for w in self.watchers for m in w.of_type("agent_error")]
        crashed = [m for w in self.watchers for m in w.of_type("crashed")]
        r.data["end"] = {"turn": g["turn"], "phase": g["phase"], "winner": g["winner"], "victory": g["victory"]}
        r.step("the game ends cleanly", g["phase"] != "playing" and not g.get("crashed") and not errors
               and not crashed, f"turn {g['turn']}, {g['phase']}, winner {g['winner']} by {g['victory']}; "
               f"{len(errors)} agent errors, {len(crashed)} crashes")
        r.step("the model seat's chat was answered", bool(self.chat) and bool(self.chat.get("negotiation")))
        r.step("the restart happened mid-game", self.restarted)
        worst = max((s.get("max_s", 0.0) for s in self.save_lock), default=0.0)
        runs = {s["run"] for s in self.save_lock}
        saves = sum(max(s.get("saves", 0) for s in self.save_lock if s["run"] == run) for run in runs)
        r.step("the save lock held at most 10 ms", 0 < worst <= SAVE_LOCK_BUDGET_S,
               f"max {worst * 1000:.2f} ms over {saves} saves (both server runs)")
        r.step("no error recorded by the session", all(s.get("errors", 1) == 0 for s in self.save_lock))

    def check_broadcasts(self):
        """Every bot turn the game's metrics record was announced by a ``turn`` broadcast."""
        rows = self.client.get(f"/api/games/{self.gid}/metrics").json()["turns"]
        bot_turns = {(row["turn"], row["player"]) for row in rows if row.get("controller") == "bot"}
        announced = {(t, p) for w in self.watchers for (t, p, _phase) in w.turns()}
        unwatched = set()
        if self.unwatched_from is not None:
            # Only when the model seat left before the restart: the turns between the kill and the new socket.
            back = min((t for (t, _p, _phase) in self.watchers[-1].turns()), default=self.unwatched_from)
            unwatched = {(t, p) for (t, p) in bot_turns if self.unwatched_from <= t <= back}
        missing = sorted(bot_turns - announced - unwatched)
        msgs = sum(len(w.messages) for w in self.watchers)
        self.report.data["broadcasts"] = {"messages": msgs, "turn_broadcasts": len(announced),
                                          "bot_turns": len(bot_turns), "unwatched": len(unwatched),
                                          "missing": missing[:20]}
        self.report.step("every bot turn was broadcast", bool(bot_turns) and not missing,
                         f"{len(bot_turns)} bot turns, {len(announced)} turns announced in {msgs} messages; "
                         f"missing {missing[:5]}")

    def check_replay(self):
        """The finished game's replay loads, every turn of it."""
        t0 = time.perf_counter()
        rp = self.client.get(f"/api/games/{self.gid}/replay", params={"token": self.spectator})
        took = time.perf_counter() - t0
        ok = rp.status_code == 200
        frames, first, last = 0, None, None
        if ok:
            body = rp.json()
            fr = body.get("frames") or []
            frames = len(fr)
            turns = [f.get("turn") for f in fr if isinstance(f, dict) and f.get("turn") is not None]
            first, last = (min(turns), max(turns)) if turns else (None, None)
        end = self.report.data.get("end", {}).get("turn")
        self.report.data["replay"] = {"bytes": len(rp.content), "frames": frames, "turns": [first, last],
                                      "seconds": round(took, 2)}
        self.report.step("the finished game's replay loads", ok and frames > 0 and first == 1
                         and last is not None and end is not None and last >= end - 1,
                         f"HTTP {rp.status_code}, {len(rp.content)} bytes, {frames} frames, turns {first} to "
                         f"{last}, in {took:.2f} s")


def _find_negotiation(value, nid) -> Optional[dict]:
    """The negotiation with id ``nid`` anywhere in a diplomacy answer."""
    if isinstance(value, dict):
        if value.get("id") == nid and ("status" in value or "entries" in value or "messages" in value):
            return value
        for v in value.values():
            found = _find_negotiation(v, nid)
            if found:
                return found
    elif isinstance(value, list):
        for v in value:
            found = _find_negotiation(v, nid)
            if found:
                return found
    return None


def main(argv=None) -> int:
    """Run the soak."""
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("state", help="the state directory, emptied first (never a directory someone plays from)")
    ap.add_argument("--port", type=int, default=0, help="a free port by default")
    ap.add_argument("--size", default="standard")
    ap.add_argument("--bots", type=int, default=8)
    ap.add_argument("--seed", type=int, default=2613)
    ap.add_argument("--chat-turn", type=int, default=20)
    ap.add_argument("--restart-turn", type=int, default=120)
    ap.add_argument("--timeout", type=float, default=3600.0, help="seconds to wait for the bots to finish the game")
    ap.add_argument("--json", help="write the summary here too")
    args = ap.parse_args(argv)
    report = Report()
    try:
        code = Soak(args, report).run()
    except SystemExit as e:
        report.step("the soak ran", False, str(e))
        code = 2
    summary = {"ok": code == 0, "steps": report.steps, **report.data}
    text = json.dumps(summary, indent=2, default=str)
    print("SUMMARY " + json.dumps(summary, default=str), flush=True)
    if args.json:
        Path(args.json).write_text(text + "\n", encoding="utf-8")
    return code


if __name__ == "__main__":
    sys.exit(main())
