//! VEVOR AIO segmented-LCD driver.
//!
//! Target device: USB HID 5131:2007 / product string "FBB" — the 1.8"
//! segmented LCD variant of the VEVOR AIO CPU cooler (Amazon B0F1TMW79P).
//! Full wire-format documentation is in [`PROTOCOL.md`](../../PROTOCOL.md).
//!
//! Runtime shape:
//!   * Main thread holds /dev/vevor_lcd open and writes a 65-byte HID report
//!     at 5 Hz (a 0x00 report-id prefix + 64-byte frame).
//!   * Background thread re-reads CPU (/sys/class/hwmon) and GPU (nvidia-smi
//!     subprocess, amdgpu hwmon fallback) at 1 Hz, storing the results into
//!     two AtomicU8s.
//!   * Clock (HH:MM) is resolved per write via libc::localtime_r FFI.
//!
//! No dependencies beyond libstd + libc. On SIGTERM the process dies; the
//! device's ~1 s watchdog blanks the display shortly after.

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::thread;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Protocol constants
// ---------------------------------------------------------------------------

const REPORT_LEN: usize = 64;
const OFF_CPU: usize = 3;
const OFF_GPU: usize = 13;
const OFF_HOUR: usize = 29;
const OFF_MINUTE: usize = 30;

/// 64-byte template OUT report, captured verbatim from the OEM Windows app's
/// idle-telemetry frame. The firmware only renders bytes 3/13/29/30; we copy
/// the rest so that if future firmware revisions inspect other bytes they
/// still see a plausible value.
const BASELINE: [u8; REPORT_LEN] = [
    0x00, 0x01, 0x02, 0x36, 0x00, 0x00, 0x05, 0x42, 0x28, 0x36, 0x24, 0x01, 0x64, 0x35, 0x00, 0x00,
    0x1b, 0x21, 0x28, 0x19, 0x14, 0x21, 0x3a, 0x21, 0x3a, 0x14, 0x1a, 0x04, 0x12, 0x0a, 0x25, 0x31,
    0x06, 0x1b, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

const DEFAULT_DEVICE: &str = "/dev/vevor_lcd";
const DEFAULT_INTERVAL_MS: u64 = 200;
const TEMP_READ_INTERVAL: Duration = Duration::from_secs(1);

// ---------------------------------------------------------------------------
// Shared temperature state
// ---------------------------------------------------------------------------

struct Temps {
    cpu_c: AtomicU8,
    gpu_c: AtomicU8,
}

impl Temps {
    fn new() -> Self {
        Self {
            cpu_c: AtomicU8::new(0),
            gpu_c: AtomicU8::new(0),
        }
    }
}

// ---------------------------------------------------------------------------
// CPU temperature (sysfs hwmon)
// ---------------------------------------------------------------------------

const HWMON_ROOT: &str = "/sys/class/hwmon";
const CPU_HWMON_NAMES: &[&str] = &["k10temp", "coretemp", "zenpower"];
const CPU_PREFERRED_LABEL_PREFIXES: &[&str] = &["Tctl", "Package"];

fn read_trim(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

fn hwmon_dirs() -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(HWMON_ROOT) else {
        return out;
    };
    let mut list: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    list.sort();
    for p in list {
        if let Some(name) = read_trim(&p.join("name")) {
            out.push((name, p));
        }
    }
    out
}

/// Locate the best `tempN_input` file for CPU readings.
fn find_cpu_sensor() -> Option<PathBuf> {
    let mons = hwmon_dirs();
    for want in CPU_HWMON_NAMES {
        let Some((_, dir)) = mons.iter().find(|(n, _)| n == want) else {
            continue;
        };
        if let Ok(entries) = fs::read_dir(dir) {
            let mut labels: Vec<PathBuf> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.file_name()
                        .and_then(|s| s.to_str())
                        .is_some_and(|s| s.starts_with("temp") && s.ends_with("_label"))
                })
                .collect();
            labels.sort();
            for label_path in &labels {
                let Some(label) = read_trim(label_path) else {
                    continue;
                };
                if CPU_PREFERRED_LABEL_PREFIXES
                    .iter()
                    .any(|p| label.starts_with(p))
                {
                    let fname = label_path.file_name()?.to_str()?;
                    let input_name = fname.replace("_label", "_input");
                    let input_path = label_path.with_file_name(input_name);
                    if input_path.exists() {
                        return Some(input_path);
                    }
                }
            }
        }
        // Fallback: first temp*_input in this hwmon directory.
        if let Some(p) = first_temp_input(dir) {
            return Some(p);
        }
    }
    None
}

fn first_temp_input(dir: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    let mut inputs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.starts_with("temp") && s.ends_with("_input"))
        })
        .collect();
    inputs.sort();
    inputs.into_iter().next()
}

fn read_millidegrees(path: &Path) -> Option<i32> {
    read_trim(path)?.parse().ok()
}

fn read_cpu_temp(sensor: Option<&Path>) -> Option<u8> {
    let raw = read_millidegrees(sensor?)?;
    Some(clamp_u8((raw + 500) / 1000))
}

// ---------------------------------------------------------------------------
// GPU temperature (nvidia-smi → amdgpu fallback)
// ---------------------------------------------------------------------------

fn read_gpu_temp_nvidia() -> Option<u8> {
    let out = Command::new("nvidia-smi")
        .args([
            "--query-gpu=temperature.gpu",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let first = text.lines().next()?.trim();
    let val: i32 = first.parse().ok()?;
    Some(clamp_u8(val))
}

fn read_gpu_temp_amdgpu() -> Option<u8> {
    let dir = hwmon_dirs()
        .into_iter()
        .find(|(n, _)| n == "amdgpu")
        .map(|(_, p)| p)?;
    let input = first_temp_input(&dir)?;
    let raw = read_millidegrees(&input)?;
    Some(clamp_u8((raw + 500) / 1000))
}

fn read_gpu_temp() -> Option<u8> {
    read_gpu_temp_nvidia().or_else(read_gpu_temp_amdgpu)
}

fn clamp_u8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

// ---------------------------------------------------------------------------
// Local wall-clock time via libc::localtime_r
// ---------------------------------------------------------------------------

// glibc `struct tm`, including the GNU extensions (`tm_gmtoff`, `tm_zone`).
// We never read those fields, but the struct must be wide enough for libc to
// write into without trampling our stack.
#[repr(C)]
struct CTm {
    tm_sec: i32,
    tm_min: i32,
    tm_hour: i32,
    tm_mday: i32,
    tm_mon: i32,
    tm_year: i32,
    tm_wday: i32,
    tm_yday: i32,
    tm_isdst: i32,
    tm_gmtoff: i64,
    tm_zone: *const i8,
}

unsafe extern "C" {
    fn time(t: *mut i64) -> i64;
    fn localtime_r(t: *const i64, out: *mut CTm) -> *mut CTm;
}

fn localtime_hm() -> (u8, u8) {
    let mut now: i64 = 0;
    // SAFETY: `time` writes a single i64 to the pointer we pass.
    let ts = unsafe { time(&mut now as *mut i64) };
    let mut tm = CTm {
        tm_sec: 0,
        tm_min: 0,
        tm_hour: 0,
        tm_mday: 0,
        tm_mon: 0,
        tm_year: 0,
        tm_wday: 0,
        tm_yday: 0,
        tm_isdst: 0,
        tm_gmtoff: 0,
        tm_zone: std::ptr::null(),
    };
    // SAFETY: both pointers reference valid, exclusive stack allocations.
    let res = unsafe { localtime_r(&ts as *const i64, &mut tm as *mut CTm) };
    if res.is_null() {
        return (0, 0);
    }
    (clamp_u8(tm.tm_hour), clamp_u8(tm.tm_min))
}

// ---------------------------------------------------------------------------
// Frame building
// ---------------------------------------------------------------------------

fn build_frame(cpu: u8, gpu: u8, hour: u8, minute: u8) -> [u8; REPORT_LEN + 1] {
    let mut buf = [0u8; REPORT_LEN + 1];
    buf[1..].copy_from_slice(&BASELINE);
    buf[1 + OFF_CPU] = cpu;
    buf[1 + OFF_GPU] = gpu;
    buf[1 + OFF_HOUR] = hour;
    buf[1 + OFF_MINUTE] = minute;
    buf
}

// ---------------------------------------------------------------------------
// Threads
// ---------------------------------------------------------------------------

fn reader_thread(temps: Arc<Temps>) {
    let cpu_sensor = find_cpu_sensor();
    // One immediate read so the first frame isn't zeros.
    refresh_temps(&temps, cpu_sensor.as_deref());
    loop {
        thread::sleep(TEMP_READ_INTERVAL);
        refresh_temps(&temps, cpu_sensor.as_deref());
    }
}

fn refresh_temps(temps: &Temps, cpu_sensor: Option<&Path>) {
    if let Some(v) = read_cpu_temp(cpu_sensor) {
        temps.cpu_c.store(v, Ordering::Relaxed);
    }
    if let Some(v) = read_gpu_temp() {
        temps.gpu_c.store(v, Ordering::Relaxed);
    }
}

/// How many consecutive failed writes we tolerate before giving up. At 200 ms
/// cadence that's ~1 s of failures — enough to ride through a quick hiccup,
/// short enough that a real unplug causes systemd to restart us promptly.
const MAX_CONSECUTIVE_WRITE_ERRORS: u32 = 5;

fn writer_loop(dev: &mut File, interval: Duration, temps: &Temps) -> std::io::Result<()> {
    let mut consecutive_errors: u32 = 0;
    loop {
        let cpu = temps.cpu_c.load(Ordering::Relaxed);
        let gpu = temps.gpu_c.load(Ordering::Relaxed);
        let (h, m) = localtime_hm();
        let buf = build_frame(cpu, gpu, h, m);
        match dev.write_all(&buf) {
            Ok(()) => consecutive_errors = 0,
            Err(e) => {
                consecutive_errors += 1;
                if consecutive_errors >= MAX_CONSECUTIVE_WRITE_ERRORS {
                    // Device is probably gone (unplugged / re-enumerated).
                    // Exit non-zero so systemd's Restart=on-failure can take
                    // over; the watchdog will have blanked the LCD by now.
                    return Err(e);
                }
            }
        }
        thread::sleep(interval);
    }
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

struct Args {
    device: String,
    interval: Duration,
}

fn parse_args() -> Result<Args, String> {
    let mut device = DEFAULT_DEVICE.to_string();
    let mut interval_ms: u64 = DEFAULT_INTERVAL_MS;

    let mut it = env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            "--device" => {
                device = it
                    .next()
                    .ok_or_else(|| "--device requires a value".to_string())?;
            }
            s if s.starts_with("--device=") => {
                device = s["--device=".len()..].to_string();
            }
            "--interval" => {
                let v = it
                    .next()
                    .ok_or_else(|| "--interval requires a value (ms)".to_string())?;
                interval_ms = v.parse().map_err(|e| format!("--interval: {e}"))?;
            }
            s if s.starts_with("--interval=") => {
                interval_ms = s["--interval=".len()..]
                    .parse()
                    .map_err(|e| format!("--interval: {e}"))?;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    if interval_ms == 0 || interval_ms >= 1000 {
        return Err(format!(
            "--interval must be 1..999 ms (device watchdog blanks at ~1 s); got {interval_ms}"
        ));
    }

    Ok(Args {
        device,
        interval: Duration::from_millis(interval_ms),
    })
}

fn print_help() {
    let prog = env::args()
        .next()
        .unwrap_or_else(|| "vevor-lcd".to_string());
    println!(
        "vevor-lcd — minimal driver for the VEVOR segmented-LCD AIO (5131:2007)\n\
         \n\
         Usage: {prog} [--device PATH] [--interval MS]\n\
         \n\
         Options:\n\
           --device PATH   hidraw node to write (default: {DEFAULT_DEVICE})\n\
           --interval MS   milliseconds between frames (default: {DEFAULT_INTERVAL_MS};\n\
                           must be 1..999 because the device watchdog blanks\n\
                           the LCD after ~1 s of silence)\n\
           -h, --help      show this help and exit\n"
    );
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("vevor-lcd: {e}");
            eprintln!("try `vevor-lcd --help`");
            return ExitCode::from(2);
        }
    };

    let mut dev = match OpenOptions::new().write(true).open(&args.device) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("vevor-lcd: cannot open {}: {}", args.device, e);
            return ExitCode::from(2);
        }
    };

    let temps = Arc::new(Temps::new());
    {
        let temps = Arc::clone(&temps);
        thread::Builder::new()
            .name("vevor-lcd-reader".into())
            .spawn(move || reader_thread(temps))
            .expect("spawn reader thread");
    }

    if let Err(e) = writer_loop(&mut dev, args.interval, &temps) {
        eprintln!("vevor-lcd: device write failed, exiting: {e}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
