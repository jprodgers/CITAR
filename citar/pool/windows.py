"""Availability windows: when a shared server may be used, in its owner's wall clock.

The brief's example is the whole problem in one sentence: "only available for scenarios from
midnight to 6am with a budget of $100 per month, their local time automatically calculated by the
server and translated for people in other time zones."

So a window is stored as a weekday plus minutes past midnight, **in the owning account's IANA zone**,
and resolved through that zone whenever it is evaluated. It is not stored as an instant, and not
stored in UTC. That distinction is the entire design:

*Storing UTC offsets breaks twice a year.* An owner in New York who says "midnight to 6am" means
their midnight. Convert that to 04:00–10:00 UTC in January and it silently becomes 23:00–05:00 local
in July, because the offset changed and the stored instant did not.

*Storing wall-clock times and resolving late is DST-correct by construction.* Each evaluation asks
what the owner's clock says right now. On the spring-forward day a midnight-to-6am window is five
hours long, and on the autumn day it is seven, which is exactly what "midnight to 6am" means to the
person who wrote it.

A window that wraps past midnight (22:00–02:00) is split into two rows at save time, so evaluation
never has to reason about a range that ends before it starts.

Everything returned is timezone-aware UTC. Rendering into a viewer's zone happens at the edge, in
`describe_for_viewer`, which is what "translated for people in other time zones" means.
"""
from __future__ import annotations

import logging
from dataclasses import dataclass
from datetime import datetime, time, timedelta, timezone
from typing import Iterable, Optional, Sequence
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

log = logging.getLogger("citar.windows")

DAY_NAMES = ("Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday")
DAY_SHORT = ("Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun")
MINUTES_PER_DAY = 24 * 60
#: How far ahead to look for the next opening. Eight days covers any weekly pattern plus a day of
#: slack, and bounds the search so a grant with no windows at all cannot spin.
HORIZON_DAYS = 8


def zone(tz: Optional[str]) -> ZoneInfo:
    """An IANA zone, falling back to UTC rather than raising.

    Time zones arrive from browsers and from stored account settings, so a bad one is a data
    problem, not a reason to fail a request that is otherwise fine.
    """
    try:
        return ZoneInfo(tz or "UTC")
    except (ZoneInfoNotFoundError, ValueError, TypeError):
        log.warning("unknown time zone %r, using UTC", tz)
        return ZoneInfo("UTC")


def valid_zone(tz: Optional[str]) -> bool:
    """Whether a string names a time zone this machine knows."""
    try:
        ZoneInfo(tz or "")
        return True
    except (ZoneInfoNotFoundError, ValueError, TypeError):
        return False


# ---------------------------------------------------------------------------- storage helpers

def normalize(weekday: int, start_min: int, end_min: int) -> list:
    """Turn one user-entered window into the rows to store.

    A window that wraps midnight becomes two rows — 22:00–02:00 on Monday is Monday 22:00–24:00 plus
    Tuesday 00:00–02:00 — so that every stored row satisfies start < end and evaluation is a simple
    containment test.
    """
    weekday = int(weekday) % 7
    start_min = max(0, min(MINUTES_PER_DAY, int(start_min)))
    end_min = max(0, min(MINUTES_PER_DAY, int(end_min)))
    if start_min == end_min:
        return []                                   # zero-length: means nothing, store nothing
    if start_min < end_min:
        return [(weekday, start_min, end_min)]
    return [(weekday, start_min, MINUTES_PER_DAY), ((weekday + 1) % 7, 0, end_min)]


def normalize_many(entries: Iterable) -> list:
    """Normalize and merge a set of windows, so overlapping rows do not double-count."""
    rows: list = []
    for entry in entries:
        if isinstance(entry, dict):
            rows += normalize(entry.get("weekday", 0), entry.get("start_min", 0), entry.get("end_min", 0))
        else:
            rows += normalize(*entry)
    return _merge(rows)


def _merge(rows: Sequence) -> list:
    """Merge touching or overlapping windows on the same weekday."""
    out: list = []
    for weekday in range(7):
        spans = sorted((s, e) for d, s, e in rows if d == weekday)
        for start, end in spans:
            if out and out[-1][0] == weekday and start <= out[-1][2]:
                out[-1] = (weekday, out[-1][1], max(out[-1][2], end))
            else:
                out.append((weekday, start, end))
    return out


def _as_tuples(windows) -> list:
    """Accept either ORM rows or plain tuples/dicts."""
    rows = []
    for w in windows or []:
        if isinstance(w, tuple):
            rows.append((int(w[0]) % 7, int(w[1]), int(w[2])))
        elif isinstance(w, dict):
            rows.append((int(w["weekday"]) % 7, int(w["start_min"]), int(w["end_min"])))
        else:
            rows.append((int(w.weekday) % 7, int(w.start_min), int(w.end_min)))
    return rows


# ---------------------------------------------------------------------------- evaluation

@dataclass
class WindowState:
    """Whether the thing is usable now, and when that next changes. Times are aware UTC."""
    open: bool
    #: When the current open period ends. None when open with no windows (always available).
    until: Optional[datetime] = None
    #: When it next opens, if currently closed.
    next_open: Optional[datetime] = None
    #: Human-readable, safe to show the person who was refused.
    reason: str = ""
    always: bool = False

    def seconds_until_change(self, now: Optional[datetime] = None) -> Optional[float]:
        """How long until the current availability window opens or closes."""
        now = now or datetime.now(timezone.utc)
        target = self.until if self.open else self.next_open
        return (target - now).total_seconds() if target else None


def evaluate(windows, tz: str, now: Optional[datetime] = None) -> WindowState:
    """Is this open right now in `tz`, and when does that change?

    No windows means no restriction — an unrestricted grant is the common case and must not require
    somebody to enter 7 rows covering the whole week.
    """
    rows = _as_tuples(windows)
    if not rows:
        return WindowState(open=True, always=True, reason="Available at any time.")

    now = (now or datetime.now(timezone.utc)).astimezone(timezone.utc)
    z = zone(tz)
    local = now.astimezone(z)
    minute = local.hour * 60 + local.minute

    open_now = any(d == local.weekday() and s <= minute < e for d, s, e in rows)

    # Boundaries are built as local wall-clock instants and converted individually, which is what
    # keeps the arithmetic correct across a DST change.
    boundaries = sorted(_boundaries(rows, local, z))
    closes = next((b for b, is_open in boundaries if b > now and not is_open), None)
    opens = next((b for b, is_open in boundaries if b > now and is_open), None)

    if open_now:
        return WindowState(open=True, until=closes,
                           reason=f"Open until {_fmt(closes, z)}." if closes else "Open.")
    return WindowState(open=False, next_open=opens,
                       reason=(f"Not available right now. Next opens {_fmt(opens, z)}"
                               f" ({_relative(opens, now)})." if opens
                               else "Not available, and no upcoming window."))


def _boundaries(rows, local_now: datetime, z: ZoneInfo) -> list:
    """Every window edge in the search horizon, as (utc instant, opens?)."""
    out = []
    start_day = local_now.date() - timedelta(days=1)   # yesterday, to catch a window still running
    for offset in range(HORIZON_DAYS + 1):
        day = start_day + timedelta(days=offset)
        for weekday, start_min, end_min in rows:
            if day.weekday() != weekday:
                continue
            out.append((_local_instant(day, start_min, z), True))
            out.append((_local_instant(day, end_min, z), False))
    return out


def _local_instant(day, minutes: int, z: ZoneInfo) -> datetime:
    """A local wall-clock time on `day`, as an aware UTC instant.

    1440 means midnight at the end of the day, which is midnight on the next one — expressing it
    that way avoids a 24:00 that `time()` will not accept.
    """
    extra_days, minutes = divmod(int(minutes), MINUTES_PER_DAY)
    naive = datetime.combine(day + timedelta(days=extra_days),
                             time(hour=minutes // 60, minute=minutes % 60))
    # fold=0 picks the first occurrence of an ambiguous local time (the autumn repeat), and a
    # nonexistent local time (the spring gap) is normalized forward by the conversion.
    return naive.replace(tzinfo=z, fold=0).astimezone(timezone.utc)


# ---------------------------------------------------------------------------- rendering

def _fmt(when: Optional[datetime], z: ZoneInfo) -> str:
    """A time as ``HH:MM``."""
    if when is None:
        return "—"
    local = when.astimezone(z)
    return local.strftime("%a %H:%M")


def _relative(when: Optional[datetime], now: datetime) -> str:
    """A duration as a readable phrase, such as "in 3 hours"."""
    if when is None:
        return ""
    seconds = max(0, (when - now).total_seconds())
    if seconds < 90:
        return "in under a minute"
    minutes = int(seconds // 60)
    if minutes < 60:
        return f"in {minutes} minutes"
    hours, minutes = divmod(minutes, 60)
    if hours < 24:
        return f"in {hours}h {minutes:02d}m"
    days, hours = divmod(hours, 24)
    return f"in {days}d {hours}h"


def describe(windows, tz: str) -> str:
    """The owner's own reading: 'Mon–Fri 00:00–06:00'."""
    rows = _as_tuples(windows)
    if not rows:
        return "Any time"
    by_span: dict = {}
    for weekday, start, end in sorted(rows):
        by_span.setdefault((start, end), []).append(weekday)
    parts = []
    for (start, end), days in sorted(by_span.items()):
        parts.append(f"{_day_range(days)} {_hhmm(start)}–{_hhmm(end)}")
    return ", ".join(parts)


def describe_for_viewer(windows, owner_tz: str, viewer_tz: str,
                        now: Optional[datetime] = None) -> dict:
    """Both readings of the same window, which is what the UI shows.

    "00:00–06:00 America/New_York · your 05:00–11:00 Europe/London". Showing only the owner's
    reading makes a viewer do the conversion in their head and get it wrong; showing only theirs
    hides why the window is where it is.
    """
    rows = _as_tuples(windows)
    state = evaluate(rows, owner_tz, now)
    out = {
        "owner_tz": owner_tz,
        "viewer_tz": viewer_tz,
        "owner_text": describe(rows, owner_tz),
        "same_zone": _same_offset(owner_tz, viewer_tz, now),
        "open": state.open,
        "always": state.always,
        "reason": state.reason,
        "until": state.until.isoformat() if state.until else None,
        "next_open": state.next_open.isoformat() if state.next_open else None,
    }
    if not rows:
        out["viewer_text"] = "Any time"
        return out
    # Translate by converting each window's edges through a real date, so the viewer's text is
    # correct for the DST rules of *both* zones rather than by adding a fixed offset.
    now = (now or datetime.now(timezone.utc)).astimezone(timezone.utc)
    owner_zone, viewer_zone = zone(owner_tz), zone(viewer_tz)
    reference = now.astimezone(owner_zone).date()
    translated = []
    for weekday, start, end in sorted(rows):
        day = reference + timedelta(days=(weekday - reference.weekday()) % 7)
        s_local = _local_instant(day, start, owner_zone).astimezone(viewer_zone)
        e_local = _local_instant(day, end, owner_zone).astimezone(viewer_zone)
        translated.append((s_local.weekday(), s_local.hour * 60 + s_local.minute,
                           e_local.hour * 60 + e_local.minute or MINUTES_PER_DAY))
    out["viewer_text"] = describe(translated, viewer_tz)
    return out


def _same_offset(a: str, b: str, now: Optional[datetime] = None) -> bool:
    """Whether two moments share a UTC offset.

    The check that catches a window spanning a daylight-saving change, where the same wall-clock time
    is a different instant on either side.
    """
    now = now or datetime.now(timezone.utc)
    return now.astimezone(zone(a)).utcoffset() == now.astimezone(zone(b)).utcoffset()


def _day_range(days: list) -> str:
    """The instants a weekday window covers on a given date."""
    days = sorted(set(days))
    if len(days) == 7:
        return "Every day"
    if days == [0, 1, 2, 3, 4]:
        return "Mon–Fri"
    if days == [5, 6]:
        return "Sat–Sun"
    # Collapse runs of consecutive days: [0,1,2,4] -> "Mon–Wed, Fri"
    runs, run = [], [days[0]]
    for d in days[1:]:
        if d == run[-1] + 1:
            run.append(d)
        else:
            runs.append(run)
            run = [d]
    runs.append(run)
    return ", ".join(DAY_SHORT[r[0]] if len(r) == 1 else f"{DAY_SHORT[r[0]]}–{DAY_SHORT[r[-1]]}"
                     for r in runs)


def _hhmm(minutes: int) -> str:
    """Minutes since midnight as ``HH:MM``."""
    if minutes >= MINUTES_PER_DAY:
        return "24:00"
    return f"{minutes // 60:02d}:{minutes % 60:02d}"


def to_client(windows, owner_tz: str) -> list:
    """A window as the client shows it, in the owner's time zone."""
    return [{"weekday": d, "start_min": s, "end_min": e,
             "label": f"{DAY_SHORT[d]} {_hhmm(s)}–{_hhmm(e)}"}
            for d, s, e in sorted(_as_tuples(windows))]
