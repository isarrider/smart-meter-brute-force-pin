#!/usr/bin/env python3
"""Brute-force a smart meter's optical PIN to unlock its extended SML data set.

Drives an optical read/write head (Schreib-Lese-Kopf) via the UART TX line and
uses the meter's SML response as an oracle: a correct PIN unlocks additional
OBIS values, which makes the periodically broadcast SML message longer than the
baseline measured before any PIN is entered.

All timing, the search space, and the oracle thresholds are configurable via
environment variables so the blink grammar can be matched to a specific meter
without editing code. Progress is checkpointed so the run resumes after a
restart instead of starting over from the first PIN.

NOTE: entering the PIN unlocks *your own* consumption data on *your own* meter.
Before running a full sweep, confirm your meter does not lock out or rate-limit
PIN entry after a few wrong attempts — if it does, a brute-force sweep is
infeasible and could leave the meter locked.
"""

import os
import sys
import time
import serial


# --- Configuration (override via environment) --------------------------------
COM_PORT = os.environ.get("COM_PORT", "/dev/ttyAMA0")
BAUDRATE = int(os.environ.get("BAUDRATE", "9600"))

# Blink grammar (all times in seconds unless noted).
PULSE_MS = float(os.environ.get("PULSE_MS", "100"))          # LED-on per blink
BLINK_GAP_S = float(os.environ.get("BLINK_GAP_S", "0.2"))    # gap between blinks within a digit
DIGIT_GAP_S = float(os.environ.get("DIGIT_GAP_S", "4.0"))    # gap between digits
# Gap after the two-blink "start of attempt" marker. Defaults to BLINK_GAP_S,
# which reproduces the original (untested) behaviour EXACTLY. CAUTION: with this
# default the start marker is spaced identically to the blinks of the first
# digit, so the meter likely reads "start + digit N" as a single digit "N+2".
# If your meter needs the marker delimited, set START_GAP_S to DIGIT_GAP_S
# (4.0). Verify against your meter's PIN-entry spec before a full sweep.
START_GAP_S = float(os.environ.get("START_GAP_S", str(BLINK_GAP_S)))

# PIN search space. PIN_START/PIN_END are inclusive. Default covers every
# 4-digit PIN, 0000 through 9999 (the original skipped 0000).
PIN_DIGITS = int(os.environ.get("PIN_DIGITS", "4"))
PIN_START = int(os.environ.get("PIN_START", "0"))
PIN_END = int(os.environ.get("PIN_END", str(10 ** PIN_DIGITS - 1)))

# Oracle thresholds.
BASELINE_SAMPLES = int(os.environ.get("BASELINE_SAMPLES", "5"))  # frames measured for baseline
LENGTH_MARGIN = int(os.environ.get("LENGTH_MARGIN", "1"))        # bytes over max baseline => candidate
READ_TIMEOUT_S = float(os.environ.get("READ_TIMEOUT_S", "8.0"))  # max wait for one SML frame

# Where to persist the last-tried PIN so a restart resumes instead of restarting.
CHECKPOINT = os.environ.get("CHECKPOINT", "/data/last_pin.txt")

# Pre-compute the byte count for one LED-on pulse.
# The head's LED is driven by the UART TX line: sending 0x00 holds TX low = LED
# on (active-low). At BAUDRATE, each byte occupies 10 bit-times (8 data + start
# + stop), so one byte ~= 10/BAUDRATE seconds.
PULSE_BYTES = b"\x00" * max(1, int(BAUDRATE * (PULSE_MS / 1000.0) / 10))

# SML transport v1 framing markers.
SML_START = b"\x1b\x1b\x1b\x1b\x01\x01\x01\x01"
SML_ESC = b"\x1b\x1b\x1b\x1b"


def pulse_on():
    """Drive TX low (LED on) for one pulse. flush() blocks until every byte has
    been clocked out; TX then returns to idle (mark/high), turning the LED off."""
    ser.write(PULSE_BYTES)
    ser.flush()


def enter_pin(number_str):
    """Send one PIN attempt as optical blinks.

    A two-blink marker opens the attempt, then each digit is sent as N blinks
    (digit 0 = no blinks, only the inter-digit gap) separated by DIGIT_GAP_S.
    """
    # Start-of-attempt marker.
    pulse_on()
    time.sleep(BLINK_GAP_S)
    pulse_on()
    time.sleep(START_GAP_S)

    for digit in number_str:
        for _ in range(int(digit)):
            pulse_on()
            time.sleep(BLINK_GAP_S)
        time.sleep(DIGIT_GAP_S)


def read_sml_message(timeout=READ_TIMEOUT_S):
    """Read one complete SML transport frame and return its length in bytes,
    or None if no complete frame arrived within `timeout`.

    Deterministic: frames are delimited by the SML start/end escape sequences,
    not by inter-byte timing, so back-to-back broadcasts cannot be merged into
    one reading.
    """
    deadline = time.monotonic() + timeout
    buf = bytearray()
    start = -1

    while time.monotonic() < deadline:
        chunk = ser.read(256)
        if not chunk:
            continue
        buf.extend(chunk)

        if start < 0:
            start = buf.find(SML_START)
            if start < 0:
                # No start marker yet; keep only a tail that might hold a
                # partial marker so the buffer can't grow without bound.
                if len(buf) > len(SML_START):
                    del buf[: -len(SML_START)]
                continue

        # Scan for the end escape (1b1b1b1b 1a + pad + CRC16 = 8 bytes) after
        # the start marker, skipping escaped payload (1b1b1b1b 1b1b1b1b).
        esc = buf.find(SML_ESC, start + len(SML_START))
        while esc >= 0:
            if esc + 8 > len(buf):
                break  # need more bytes to classify this escape
            if buf[esc + 4] == 0x1A:
                return (esc + 8) - start
            esc = buf.find(SML_ESC, esc + 4)

    return None


def measure(label):
    """Flush stale input, then measure the length of the next fresh SML frame."""
    ser.reset_input_buffer()
    length = read_sml_message()
    if length is None:
        print(f"{label}: no SML frame within {READ_TIMEOUT_S}s", flush=True)
    return length


def load_checkpoint():
    try:
        with open(CHECKPOINT) as fh:
            return int(fh.read().strip())
    except (FileNotFoundError, ValueError):
        return None


def save_checkpoint(number):
    try:
        os.makedirs(os.path.dirname(CHECKPOINT) or ".", exist_ok=True)
        tmp = CHECKPOINT + ".tmp"
        with open(tmp, "w") as fh:
            fh.write(str(number))
        os.replace(tmp, CHECKPOINT)  # atomic
    except OSError as exc:
        print(f"WARNING: could not write checkpoint {CHECKPOINT}: {exc}", flush=True)


# --- Main --------------------------------------------------------------------
ser = serial.Serial(COM_PORT, BAUDRATE, timeout=0.5)

# Baseline: several frames, so normal length jitter in the meter's broadcasts
# does not trigger a false positive. A candidate must exceed the largest
# baseline frame by at least LENGTH_MARGIN bytes.
print(f"Measuring {BASELINE_SAMPLES} baseline frames...", flush=True)
baseline = []
for i in range(BASELINE_SAMPLES):
    length = measure(f"baseline {i + 1}/{BASELINE_SAMPLES}")
    if length is not None:
        baseline.append(length)
        print(f"  baseline frame {i + 1}: {length} bytes", flush=True)

if not baseline:
    print("ERROR: no baseline SML frames received; check wiring and port.", flush=True)
    ser.close()
    sys.exit(1)

baseline_max = max(baseline)
threshold = baseline_max + LENGTH_MARGIN
print(f"Baseline lengths {sorted(set(baseline))}; hit threshold >= {threshold} bytes", flush=True)

# Resume from the checkpoint if present.
resume_from = PIN_START
last = load_checkpoint()
if last is not None and PIN_START <= last < PIN_END:
    resume_from = last + 1
    print(f"Resuming after checkpoint {last:0{PIN_DIGITS}d}", flush=True)

found = None
for number in range(resume_from, PIN_END + 1):
    number_str = format(number, f"0{PIN_DIGITS}d")

    enter_pin(number_str)
    length = measure(f"PIN {number_str}")
    save_checkpoint(number)

    if length is not None and length >= threshold:
        # Candidate: confirm by re-entering the same PIN once. A real unlock
        # repeats; a one-off length fluctuation does not.
        print(f"Candidate PIN={number_str} ({length} bytes) -- confirming...", flush=True)
        enter_pin(number_str)
        confirm = measure(f"confirm {number_str}")
        if confirm is not None and confirm >= threshold:
            found = number_str
            break
        print(f"  not confirmed ({confirm} bytes); continuing", flush=True)
        continue

    shown = length if length is not None else "-"
    print(f"Tried {number_str} -- {shown} bytes", flush=True)

ser.close()

if found:
    print(f"*** PIN={found} ***", flush=True)
    sys.exit(0)
else:
    print("Search space exhausted; no PIN unlocked a longer response.", flush=True)
    sys.exit(2)
