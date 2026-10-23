//! Brute-force a smart meter's optical PIN to unlock its extended SML data set.
//!
//! Drives an optical read/write head (Schreib-Lese-Kopf) via the UART TX line
//! and uses the meter's SML response as an oracle: a correct PIN unlocks
//! additional OBIS values, which makes the periodically broadcast SML message
//! longer than the baseline measured before any PIN is entered.
//!
//! All timing, the search space, and the oracle thresholds are configurable via
//! environment variables so the blink grammar can be matched to a specific meter
//! without rebuilding. Progress is checkpointed so the run resumes after a
//! restart instead of starting over from the first PIN.
//!
//! NOTE: entering the PIN unlocks *your own* consumption data on *your own*
//! meter. Before running a full sweep, confirm your meter does not lock out or
//! rate-limit PIN entry after a few wrong attempts.

use std::env;
use std::fs;
use std::path::Path;
use std::process::ExitCode;
use std::str::FromStr;
use std::thread::sleep;
use std::time::{Duration, Instant};

use serial2::SerialPort;

/// SML transport v1 framing markers.
const SML_START: &[u8] = &[0x1b, 0x1b, 0x1b, 0x1b, 0x01, 0x01, 0x01, 0x01];
const SML_ESC: &[u8] = &[0x1b, 0x1b, 0x1b, 0x1b];

/// Poll the serial port in short slices so `read_sml_message` can re-check its
/// own overall deadline between reads.
const POLL_SLICE: Duration = Duration::from_millis(250);

struct Config {
    com_port: String,
    baudrate: u32,
    pulse_ms: f64,
    blink_gap: f64,
    digit_gap: f64,
    start_gap: f64,
    pin_digits: usize,
    pin_start: u32,
    pin_end: u32,
    baseline_samples: usize,
    length_margin: usize,
    read_timeout: f64,
    checkpoint: String,
}

fn env_or<T: FromStr>(key: &str, default: T) -> T {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

impl Config {
    fn from_env() -> Self {
        let pin_digits: usize = env_or("PIN_DIGITS", 4);
        let default_end: u32 = 10u32
            .checked_pow(pin_digits as u32)
            .map(|v| v - 1)
            .unwrap_or(u32::MAX);
        let blink_gap: f64 = env_or("BLINK_GAP_S", 0.2);
        Config {
            com_port: env::var("COM_PORT").unwrap_or_else(|_| "/dev/ttyAMA0".into()),
            baudrate: env_or("BAUDRATE", 9600),
            pulse_ms: env_or("PULSE_MS", 100.0),
            blink_gap,
            digit_gap: env_or("DIGIT_GAP_S", 4.0),
            // Defaults to BLINK_GAP_S, reproducing the original (untested)
            // spacing exactly. See README caution: the start marker may need
            // delimiting (set START_GAP_S=4.0) depending on the meter.
            start_gap: env_or("START_GAP_S", blink_gap),
            pin_digits,
            pin_start: env_or("PIN_START", 0),
            pin_end: env_or("PIN_END", default_end),
            baseline_samples: env_or("BASELINE_SAMPLES", 5),
            length_margin: env_or("LENGTH_MARGIN", 1),
            read_timeout: env_or("READ_TIMEOUT_S", 8.0),
            checkpoint: env::var("CHECKPOINT").unwrap_or_else(|_| "/data/last_pin.txt".into()),
        }
    }
}

/// Drive TX low (LED on) for one pulse. `flush` blocks until every byte has been
/// clocked out; TX then returns to idle (mark/high), turning the LED off.
fn pulse_on(port: &SerialPort, pulse: &[u8]) -> std::io::Result<()> {
    port.write_all(pulse)?;
    port.flush()
}

/// Send one PIN attempt as optical blinks: a two-blink start marker, then each
/// digit as N blinks (digit 0 = no blinks, only the inter-digit gap).
fn enter_pin(
    port: &SerialPort,
    pulse: &[u8],
    number_str: &str,
    cfg: &Config,
) -> std::io::Result<()> {
    pulse_on(port, pulse)?;
    sleep(Duration::from_secs_f64(cfg.blink_gap));
    pulse_on(port, pulse)?;
    sleep(Duration::from_secs_f64(cfg.start_gap));

    for ch in number_str.chars() {
        let digit = ch.to_digit(10).unwrap_or(0);
        for _ in 0..digit {
            pulse_on(port, pulse)?;
            sleep(Duration::from_secs_f64(cfg.blink_gap));
        }
        sleep(Duration::from_secs_f64(cfg.digit_gap));
    }
    Ok(())
}

/// First index of `needle` in `hay` at or after `from`, if any.
fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from >= hay.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// Read one complete SML transport frame and return its length in bytes, or
/// `None` if no complete frame arrived within `timeout`.
///
/// Deterministic: frames are delimited by the SML start/end escape sequences,
/// not by inter-byte timing, so back-to-back broadcasts cannot be merged into
/// one reading.
fn read_sml_message(port: &SerialPort, timeout: Duration) -> Option<usize> {
    let deadline = Instant::now() + timeout;
    let mut buf: Vec<u8> = Vec::new();
    let mut start: Option<usize> = None;
    let mut tmp = [0u8; 256];

    while Instant::now() < deadline {
        match port.read(&mut tmp) {
            Ok(0) => continue,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(_) => return None,
        }

        if start.is_none() {
            match find(&buf, SML_START, 0) {
                Some(idx) => start = Some(idx),
                None => {
                    // No start marker yet; keep only a tail that might hold a
                    // partial marker so the buffer can't grow without bound.
                    if buf.len() > SML_START.len() {
                        let cut = buf.len() - SML_START.len();
                        buf.drain(..cut);
                    }
                    continue;
                }
            }
        }
        let s = start.unwrap();

        // Scan for the end escape (1b1b1b1b 1a + pad + CRC16 = 8 bytes) after
        // the start marker, skipping escaped payload (1b1b1b1b 1b1b1b1b).
        let mut esc = find(&buf, SML_ESC, s + SML_START.len());
        while let Some(e) = esc {
            if e + 8 > buf.len() {
                break; // need more bytes to classify this escape
            }
            if buf[e + 4] == 0x1a {
                return Some((e + 8) - s);
            }
            esc = find(&buf, SML_ESC, e + 4);
        }
    }

    None
}

/// Flush stale input, then measure the length of the next fresh SML frame.
fn measure(port: &SerialPort, timeout: Duration, label: &str) -> Option<usize> {
    let _ = port.discard_input_buffer();
    let result = read_sml_message(port, timeout);
    if result.is_none() {
        println!("{label}: no SML frame within {:.1}s", timeout.as_secs_f64());
    }
    result
}

fn load_checkpoint(path: &str) -> Option<u32> {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

fn save_checkpoint(path: &str, number: u32) {
    if let Some(dir) = Path::new(path).parent() {
        if !dir.as_os_str().is_empty() {
            let _ = fs::create_dir_all(dir);
        }
    }
    let tmp = format!("{path}.tmp");
    if fs::write(&tmp, number.to_string()).is_err() {
        eprintln!("WARNING: could not write checkpoint {path}");
        return;
    }
    if let Err(e) = fs::rename(&tmp, path) {
        eprintln!("WARNING: could not write checkpoint {path}: {e}");
    }
}

fn opt_len(v: Option<usize>) -> String {
    v.map(|l| l.to_string()).unwrap_or_else(|| "-".into())
}

fn main() -> ExitCode {
    let cfg = Config::from_env();

    // Byte count for one LED-on pulse. The head's LED is driven by the UART TX
    // line: sending 0x00 holds TX low = LED on (active-low). At BAUDRATE each
    // byte occupies 10 bit-times (8 data + start + stop).
    let pulse_n = ((cfg.baudrate as f64 * (cfg.pulse_ms / 1000.0) / 10.0) as usize).max(1);
    let pulse = vec![0u8; pulse_n];
    let read_timeout = Duration::from_secs_f64(cfg.read_timeout);

    let mut port = match SerialPort::open(&cfg.com_port, cfg.baudrate) {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "ERROR: cannot open {} at {} baud: {e}",
                cfg.com_port, cfg.baudrate
            );
            return ExitCode::from(1);
        }
    };
    let _ = port.set_read_timeout(POLL_SLICE);

    // Baseline: several frames, so normal length jitter in the meter's
    // broadcasts does not trigger a false positive. A candidate must exceed the
    // largest baseline frame by at least LENGTH_MARGIN bytes.
    println!("Measuring {} baseline frames...", cfg.baseline_samples);
    let mut baseline: Vec<usize> = Vec::new();
    for i in 0..cfg.baseline_samples {
        let label = format!("baseline {}/{}", i + 1, cfg.baseline_samples);
        if let Some(len) = measure(&port, read_timeout, &label) {
            baseline.push(len);
            println!("  baseline frame {}: {} bytes", i + 1, len);
        }
    }
    if baseline.is_empty() {
        println!("ERROR: no baseline SML frames received; check wiring and port.");
        return ExitCode::from(1);
    }
    let baseline_max = *baseline.iter().max().unwrap();
    let threshold = baseline_max + cfg.length_margin;
    let mut uniq = baseline.clone();
    uniq.sort_unstable();
    uniq.dedup();
    println!("Baseline lengths {uniq:?}; hit threshold >= {threshold} bytes");

    // Resume from the checkpoint if present.
    let mut resume_from = cfg.pin_start;
    if let Some(last) = load_checkpoint(&cfg.checkpoint) {
        if last >= cfg.pin_start && last < cfg.pin_end {
            resume_from = last + 1;
            println!(
                "Resuming after checkpoint {last:0width$}",
                width = cfg.pin_digits
            );
        }
    }

    let mut found: Option<String> = None;
    for number in resume_from..=cfg.pin_end {
        let number_str = format!("{number:0width$}", width = cfg.pin_digits);

        if let Err(e) = enter_pin(&port, &pulse, &number_str, &cfg) {
            eprintln!("ERROR writing PIN {number_str}: {e}");
        }
        let len = measure(&port, read_timeout, &format!("PIN {number_str}"));
        save_checkpoint(&cfg.checkpoint, number);

        if let Some(l) = len {
            if l >= threshold {
                // Candidate: confirm by re-entering the same PIN once. A real
                // unlock repeats; a one-off length fluctuation does not.
                println!("Candidate PIN={number_str} ({l} bytes) -- confirming...");
                let _ = enter_pin(&port, &pulse, &number_str, &cfg);
                let confirm = measure(&port, read_timeout, &format!("confirm {number_str}"));
                if confirm.is_some_and(|c| c >= threshold) {
                    found = Some(number_str);
                    break;
                }
                println!("  not confirmed ({} bytes); continuing", opt_len(confirm));
                continue;
            }
        }
        println!("Tried {number_str} -- {} bytes", opt_len(len));
    }

    drop(port);

    match found {
        Some(pin) => {
            println!("*** PIN={pin} ***");
            ExitCode::from(0)
        }
        None => {
            println!("Search space exhausted; no PIN unlocked a longer response.");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_len(buf: &[u8]) -> Option<usize> {
        let start = find(buf, SML_START, 0)?;
        let mut esc = find(buf, SML_ESC, start + SML_START.len());
        while let Some(e) = esc {
            if e + 8 > buf.len() {
                break;
            }
            if buf[e + 4] == 0x1a {
                return Some((e + 8) - start);
            }
            esc = find(buf, SML_ESC, e + 4);
        }
        None
    }

    fn end_escape() -> Vec<u8> {
        // 1b1b1b1b 1a + pad + CRC16 (2 bytes)
        vec![0x1b, 0x1b, 0x1b, 0x1b, 0x1a, 0x00, 0x12, 0x34]
    }

    #[test]
    fn full_frame() {
        let mut f = SML_START.to_vec();
        f.extend_from_slice(b"ABCDE");
        f.extend_from_slice(&end_escape());
        assert_eq!(frame_len(&f), Some(f.len()));
    }

    #[test]
    fn leading_garbage() {
        let mut f = SML_START.to_vec();
        f.extend_from_slice(b"ABCDE");
        f.extend_from_slice(&end_escape());
        let mut buf = b"junkjunk".to_vec();
        buf.extend_from_slice(&f);
        assert_eq!(frame_len(&buf), Some(f.len()));
    }

    #[test]
    fn escaped_payload_not_mistaken_for_end() {
        let mut f = SML_START.to_vec();
        f.extend_from_slice(b"XX");
        // escaped 1b1b1b1b 1b1b1b1b inside the payload
        f.extend_from_slice(&[0x1b, 0x1b, 0x1b, 0x1b, 0x1b, 0x1b, 0x1b, 0x1b]);
        f.extend_from_slice(b"YY");
        f.extend_from_slice(&end_escape());
        assert_eq!(frame_len(&f), Some(f.len()));
    }

    #[test]
    fn incomplete_frame_is_none() {
        let mut f = SML_START.to_vec();
        f.extend_from_slice(b"ABCDE");
        assert_eq!(frame_len(&f), None);
    }
}
