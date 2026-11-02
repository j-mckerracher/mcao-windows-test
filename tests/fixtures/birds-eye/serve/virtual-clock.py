#!/usr/bin/env python3
"""Virtual-clock proof for the bird's-eye collector schedule (DPES-798, AC7/T3).

Usage: virtual-clock.py <repo-root>

Loads bin/fm-birds-eye-view-serve.py unchanged and runs Observer._collect_forever on the
calling thread. An injected clock replaces time.monotonic, and the stop event's wait()
advances that clock instead of sleeping, so every cycle start, duration, and wait is exact
and no real time passes. Each scenario prints "ok - <name>" or "not ok - <name>: <detail>";
the exit status is 1 when any scenario fails.
"""
import importlib.util
import os
import sys
import tempfile
import time as real_time
import types

ROOT = sys.argv[1]
SERVE = os.path.join(ROOT, 'bin', 'fm-birds-eye-view-serve.py')
VIEW = os.path.join(ROOT, 'tests', 'fixtures', 'birds-eye', 'serve', 'view.json')

spec = importlib.util.spec_from_file_location('fm_birds_eye_view_serve', SERVE)
serve = importlib.util.module_from_spec(spec)
spec.loader.exec_module(serve)


class Clock:
    def __init__(self):
        self.now = 0.0

    def monotonic(self):
        return self.now

    def advance(self, seconds):
        self.now += seconds


class VirtualStop:
    """threading.Event stand-in: wait() moves the virtual clock forward instead of sleeping."""

    def __init__(self, clock, horizon):
        self.clock = clock
        self.horizon = horizon
        self.forced = False

    def set(self):
        self.forced = True

    def is_set(self):
        return self.forced or self.clock.now >= self.horizon

    def wait(self, timeout=None):
        self.clock.advance(timeout)
        return self.is_set()


def run_schedule(durations, interval, horizon, stop_in_cycle=None):
    """Drive the real collector loop; return (cycles, max concurrent cycles).

    Each cycle records its virtual start and end. A cycle advances the clock by its scripted
    duration, the way a real model run would take real time.
    """
    clock = Clock()
    # Shim the module's own time name; the process-wide time module stays real.
    serve.time = types.SimpleNamespace(monotonic=clock.monotonic, sleep=real_time.sleep)
    state_dir = tempfile.mkdtemp(prefix='virtual-clock.')
    observer = serve.Observer('model', 'capture', interval, [], state_dir)
    with open(VIEW, 'rb') as fh:
        view = fh.read()
    cycles = []
    active = [0]
    peak = [0]

    def fake_run(argv, timeout):
        index = len(cycles)
        active[0] += 1
        peak[0] = max(peak[0], active[0])
        start = clock.now
        clock.advance(durations[index % len(durations)])
        cycles.append((start, clock.now))
        active[0] -= 1
        if stop_in_cycle is not None and index + 1 == stop_in_cycle:
            observer.stop.set()
        return 0, view, b''

    observer._run = fake_run
    observer.stop = VirtualStop(clock, horizon)
    observer._collect_forever()
    return cycles, peak[0]


failures = []


def check(name, condition, detail):
    if condition:
        print(f'ok - {name}')
    else:
        failures.append(name)
        print(f'not ok - {name}: {detail}')


def starts_of(cycles):
    return [start for start, _end in cycles]


def gaps_of(cycles):
    starts = starts_of(cycles)
    return [later - earlier for earlier, later in zip(starts, starts[1:])]


def overlaps(cycles):
    """Count cycles that start before the previous cycle ended (virtual time)."""
    return sum(1 for (_s, end), (start, _e) in zip(cycles, cycles[1:]) if start < end)


# Fast cycles (1 s) on a 5 s interval start every 5 s: the wait fills the remainder.
cycles, peak = run_schedule([1], interval=5, horizon=60)
check('fast cycles start once per interval',
      starts_of(cycles) == list(range(0, 60, 5)) and overlaps(cycles) == 0 and peak == 1,
      f'starts {starts_of(cycles)}, overlaps {overlaps(cycles)}, peak {peak}')

# Slow cycles (12 s) on a 5 s interval never overlap and never burst to catch up: the next
# cycle starts when the previous one ends, and every gap is the full cycle length.
cycles, peak = run_schedule([12], interval=5, horizon=60)
check('slow cycles never overlap or catch up',
      starts_of(cycles) == [0, 12, 24, 36, 48] and gaps_of(cycles) == [12, 12, 12, 12]
      and overlaps(cycles) == 0 and peak == 1,
      f'starts {starts_of(cycles)}, gaps {gaps_of(cycles)}, overlaps {overlaps(cycles)}, peak {peak}')

# Mixed durations: each next start is exactly max(interval, previous duration) after the
# previous start, and no cycle starts before the previous one ended.
durations = [2, 9, 5, 14, 1]
interval = 5
cycles, peak = run_schedule(durations, interval=interval, horizon=120)
expected = [max(interval, durations[i % len(durations)]) for i in range(len(cycles) - 1)]
check('mixed durations follow max(interval, duration) with no overlap',
      gaps_of(cycles) == expected and overlaps(cycles) == 0 and peak == 1 and len(cycles) >= 8,
      f'gaps {gaps_of(cycles)} expected {expected}, overlaps {overlaps(cycles)}, peak {peak}')

# A stop requested inside a cycle lets that cycle finish, then ends the loop with no new start.
cycles, peak = run_schedule([1], interval=5, horizon=1000, stop_in_cycle=2)
check('a stop requested mid-cycle starts no further cycle',
      len(cycles) == 2 and overlaps(cycles) == 0 and peak == 1,
      f'cycles {cycles}, overlaps {overlaps(cycles)}, peak {peak}')

check('the production floor is five seconds', serve.MIN_INTERVAL == 5,
      f'MIN_INTERVAL is {serve.MIN_INTERVAL}')

sys.exit(1 if failures else 0)
