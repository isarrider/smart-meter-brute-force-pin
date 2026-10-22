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

## Run

	docker run -d --restart unless-stopped \
	 --device /dev/ttyAMA0:/dev/ttyAMA0 \
	 -v smbf_data:/data \
	 --name smart-meter-brute-force-pin \
	 -e PYTHONUNBUFFERED=1 \
	 isarrider/smart-meter-brute-force-pin:latest

and follow the logs with:

	docker logs -f smart-meter-brute-force-pin

or in Portainer.

The `-v smbf_data:/data` volume holds the progress checkpoint, so a restart
resumes where it left off instead of starting over from the first PIN.

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
