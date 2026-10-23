# smart-meter-brute-force-pin
Brute forces a Smart Meter PIN to unlock the meter's extended SML data set.

German smart meters (eHZ / moderne Messeinrichtung) gate the extended data set
(instantaneous power, per-phase values, ...) behind a 4-digit PIN that you enter
optically. This tool drives an optical read/write head (Schreib-Lese-Kopf) to
try PINs and watches the meter's SML response: a correct PIN unlocks additional
OBIS values, so the broadcast SML message gets longer than the baseline measured
before any PIN is entered.

Uart must be enabled in config.txt:

	#-------Serial/UART-----
	enable_uart=1

and a Smart Meter IR Read Write Dongle (Schreib Lese Kopf) has to be attached - I use this one:
https://www.ebay.de/itm/296425429208?itmmeta=01KRE6PEXYJYT9Z03Q1V2C5TQ7&hash=item45045518d8:g:0yUAAOSwpDJl9wjE

Written in Rust (`serial2`, no libudev dependency). The image builds against
musl and runs on Alpine (a small, effectively static binary).

A multiarch image (`linux/amd64`, `linux/arm64`, `linux/arm/v7`,
`linux/riscv64`) is built and pushed to GHCR by CI on every push to `main` and
on version tags, so on a Raspberry Pi (32- or 64-bit) or a RISC-V board you can
just pull it — no local build needed:

	ghcr.io/isarrider/smart-meter-brute-force-pin:latest

## Run

	docker run -d --restart unless-stopped \
	 --device /dev/ttyAMA0:/dev/ttyAMA0 \
	 -v smbf_data:/data \
	 --name smart-meter-brute-force-pin \
	 ghcr.io/isarrider/smart-meter-brute-force-pin:latest

and follow the logs with:

	docker logs -f smart-meter-brute-force-pin

or in Portainer.

The `-v smbf_data:/data` volume holds the progress checkpoint, so a restart
resumes where it left off instead of starting over from the first PIN. Output is
line-buffered, so logs stream live without any extra flags.

### Without Docker

	cargo build --release
	COM_PORT=/dev/ttyAMA0 ./target/release/smart-meter-brute-force-pin

The user running it needs access to the serial device (typically the `dialout`
group). With no `/data` volume, point the checkpoint somewhere writable, e.g.
`CHECKPOINT=./last_pin.txt`.

### Building the image yourself

Single architecture (host), e.g. straight on the Pi:

	docker build -t smart-meter-brute-force-pin:latest .

Multiarch with buildx. The binary is cross-compiled to a static musl target
(via `cargo-zigbuild`) on the native build host, so no QEMU emulation is needed:

	docker buildx build \
	 --platform linux/amd64,linux/arm64,linux/arm/v7,linux/riscv64 \
	 -t ghcr.io/isarrider/smart-meter-brute-force-pin:latest --push .

## Configuration

All behaviour is set through environment variables (`-e NAME=value`):

| Variable           | Default         | Meaning                                                       |
|--------------------|-----------------|---------------------------------------------------------------|
| `COM_PORT`         | `/dev/ttyAMA0`  | Serial port of the optical head                               |
| `BAUDRATE`         | `9600`          | Serial baud rate                                              |
| `PULSE_MS`         | `100`           | LED-on time per blink (ms)                                    |
| `BLINK_GAP_S`      | `0.2`           | Off-gap between blinks within a digit                         |
| `DIGIT_GAP_S`      | `4.0`           | Off-gap between digits                                        |
| `START_GAP_S`      | = `BLINK_GAP_S` | Off-gap after the start-of-attempt marker (see caution below) |
| `PIN_DIGITS`       | `4`             | Number of PIN digits                                          |
| `PIN_START`        | `0`             | First PIN to try (inclusive)                                  |
| `PIN_END`          | `9999`          | Last PIN to try (inclusive)                                   |
| `BASELINE_SAMPLES` | `5`             | SML frames measured for the baseline                          |
| `LENGTH_MARGIN`    | `1`             | Bytes over the largest baseline frame to count as a candidate |
| `READ_TIMEOUT_S`   | `8.0`           | Max wait for one complete SML frame                           |
| `CHECKPOINT`       | `/data/last_pin.txt` | Checkpoint file path                                     |

## How it decides a PIN is correct

* It measures several baseline SML frames first and records the largest.
* After each PIN attempt it reads one complete SML frame (delimited by the SML
  start/end escape sequences, so the measurement is deterministic rather than
  timing-based).
* A frame at least `LENGTH_MARGIN` bytes longer than the largest baseline frame
  is treated as a **candidate**, then the same PIN is entered a second time to
  confirm before the run stops. This avoids stopping on a one-off length jitter.

## Example meters and how optical PIN entry works

Most German meters (moderne Messeinrichtung) expose a single optical button —
the IR diode on the front. A flashlight toggled against it acts as that button;
this tool drives the same button electronically from the read/write head, which
is why the blink grammar below mirrors how a human would flash a torch.

**EMH mMe4.0** (and similar EMH eHZ), per the manufacturer manual:
- A short flash (< 4.5 s) on the optical button wakes it; after the display test
  the PIN prompt appears.
- Each digit starts at `0`; flashing the button counts the digit up, and after a
  short pause the meter auto-advances to the next digit (there is no explicit
  confirm button). A long flash (> 4.5 s) selects a register / ends a list.
- After the PIN, the second display line scrolls through `P` (momentary power),
  `E` (consumption, import/export separated), `1d/7d/30d/365d` history, and the
  `InF on/off` + `PIN on/off` settings.

This "N flashes per digit, pause to advance" scheme is exactly what the script
emulates (N blinks per digit, `DIGIT_GAP_S` between digits).

**Default PIN:** unconfigured meters commonly ship with `0000` — which is why
this tool now tries `0000` (the original range skipped it).

**What unlocking gives you:** the extended data set — momentary power
(import/export) and per-phase voltage L1/L2/L3 — on top of the basic total-energy
reading that the optical D0 interface serves without a PIN.

**Lockout / attempts — varies by operator and model, confirm for yours:**
- Stromnetz Berlin states their meters do not block: a wrong PIN can be
  re-entered as often as needed.
- The EMH mMe4.0 manual documents no lockout after failed attempts (it only
  requires cycling through the remaining digits before re-entering).
- This is not guaranteed for every meter — check your manufacturer's manual or
  ask your metering point operator before a full sweep.

**Getting the PIN the official way:** your metering point operator
(Messstellenbetreiber) issues it and sends it by post, usually free of charge;
on some meters it is issued once and cannot be changed.

Sources: [EMH mMe4.0 Kundenbedienungsanleitung (Stadtwerke Pforzheim)](https://www.stadtwerke-pforzheim.de/fileadmin/user_upload/Downloads/Strom/Smart_Meter/bedienungsanleitungen/Kundenbedienungsanleitung_EMH_mMe4.0.pdf) · [Stromnetz Berlin meter instructions](https://www.stromnetz.berlin/en/meters/meter-instructions/) · [stromleser.de – PIN entry on digital meters](https://stromleser.de/en/blogs/news/pin-eingabe-digitale-stromzaehler) · [recessim wiki](https://wiki.recessim.com/)

## Caution

* Entering the PIN unlocks **your own** data on **your own** meter.
* Before a full sweep, confirm your meter does not **lock out or rate-limit**
  PIN entry after a few wrong attempts. If it does, brute-forcing is infeasible
  and could leave the meter locked until the operator resets it.
* A full sweep is slow (tens of seconds per PIN, dominated by `DIGIT_GAP_S`), so
  the worst case is many hours — hence the resume-from-checkpoint behaviour.
* **`START_GAP_S` default reproduces the original, still-untested behaviour:**
  the start marker is spaced like the blinks of the first digit, so the meter
  may read "start + digit N" as a single digit. If your meter needs the marker
  delimited, set `START_GAP_S=4.0`. Verify the blink grammar against your
  meter's PIN-entry spec.

______

Still WIP, as I wanna test it...
