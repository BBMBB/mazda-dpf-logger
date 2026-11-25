mod adapter;
mod leds;
mod pids;
mod protocol;

use chrono::{SecondsFormat, Utc};
use leds::{LedState, StatusLeds};
use std::error::Error;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

// Target time between sweep starts. Queries are sequential; slow sweeps may overrun.
const POLL_INTERVAL: Duration = Duration::from_secs(10);
const DEFAULT_BAUD: u32 = 115200;

struct Options {
    port: String,
    baud: u32,
    output: PathBuf,
    leds: bool,
}

fn options() -> Result<Option<Options>, Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let mut port = None;
    let mut baud = DEFAULT_BAUD;
    let mut output = PathBuf::from("logs");
    let mut leds = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("mazda-dpf-logger {}\n\nRead-only OBD logger (services 01 and 22).\n\nUsage: mazda-dpf-logger <PORT> [--baud <BAUD>] [--output <DIRECTORY>] [--leds]\n\n  <PORT>                Serial device, preferably /dev/serial/by-id/...\n  --baud <BAUD>         Adapter serial baud rate (default: {DEFAULT_BAUD})\n  --output <DIRECTORY>  Directory for all CSV and error logs (default: logs)\n  --leds                Enable GPIO17/GPIO27 LEDs (requires pi-leds feature)\n  -h, --help            Show help\n\nStop with Ctrl+C.", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "--baud" => {
                baud = args
                    .next()
                    .ok_or("Missing value for --baud")?
                    .parse()
                    .map_err(|_| "Invalid baud rate: expected a positive integer")?;
                if baud == 0 {
                    return Err("Baud rate must be greater than zero".into());
                }
            }
            "--output" => output = args.next().ok_or("Missing value for --output")?.into(),
            "--leds" => leds = true,
            _ if arg.starts_with('-') => return Err(format!("Unknown option: {arg}").into()),
            _ if port.is_none() => port = Some(arg),
            _ => return Err(format!("Unexpected argument: {arg}").into()),
        }
    }
    Ok(Some(Options {
        port: port.ok_or("Missing serial port; use --help for usage")?,
        baud,
        output,
        leds,
    }))
}

fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn new_log(path: &std::path::Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

#[cfg(not(feature = "rtc"))]
fn new_trip_log(directory: &std::path::Path) -> io::Result<(PathBuf, File)> {
    let mut highest = 0_u64;
    for entry in fs::read_dir(directory)? {
        let name = entry?.file_name();
        let Some(name) = name.to_str().and_then(|name| name.strip_prefix("trip_")) else {
            continue;
        };
        // Reserve numbers from companion logs too, including an orphaned error log.
        let Some(number) = name
            .strip_suffix(".csv")
            .or_else(|| name.strip_suffix(".errors.log"))
        else {
            continue;
        };
        if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let number = number.parse::<u64>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Existing trip number exceeds u64",
            )
        })?;
        highest = highest.max(number);
    }
    loop {
        highest = highest
            .checked_add(1)
            .ok_or_else(|| io::Error::other("Trip numbers exhausted"))?;
        let path = directory.join(format!("trip_{highest:06}.csv"));
        match new_log(&path) {
            Ok(file) => return Ok((path, file)),
            // Another logger may have reserved this number after the directory scan.
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

fn event(log: &mut File, pid: &str, message: &str, raw: &str) -> io::Result<()> {
    writeln!(log, "{} [{pid}] {message}; raw={raw:?}", timestamp())?;
    log.flush()
}

fn run(
    options: Options,
    running: &AtomicBool,
    leds: &mut StatusLeds,
) -> Result<(), Box<dyn Error>> {
    #[cfg(not(feature = "rtc"))]
    let session_start = Instant::now();
    fs::create_dir_all(&options.output)?;
    #[cfg(feature = "rtc")]
    let stem = Utc::now().format("drive_%Y%m%dT%H%M%S%.9fZ").to_string();
    #[cfg(feature = "rtc")]
    let csv_path = options.output.join(format!("{stem}.csv"));
    #[cfg(feature = "rtc")]
    let csv_file = new_log(&csv_path)?;
    #[cfg(not(feature = "rtc"))]
    let (csv_path, csv_file) = new_trip_log(&options.output)?;
    let mut csv = csv::Writer::from_writer(csv_file);
    let mut errors = new_log(&csv_path.with_extension("errors.log"))?;
    csv.write_record(["timestamp", "pid", "name", "raw_hex", "value", "unit"])?;
    csv.flush()?;
    println!("CSV: {}", csv_path.display());

    let mut adapter = match adapter::Adapter::open(&options.port, options.baud) {
        Ok(adapter) => adapter,
        Err(error) => {
            event(
                &mut errors,
                "adapter",
                &format!("Cannot open serial port: {error}"),
                "",
            )?;
            return Err(error);
        }
    };
    if let Err(error) = adapter.initialize(running, |command, reply| {
        event(
            &mut errors,
            command,
            reply.error.as_deref().unwrap_or("Adapter initialization"),
            &reply.raw,
        )
    }) {
        event(&mut errors, "adapter", &error.to_string(), "")?;
        if !running.load(Ordering::SeqCst) {
            return Ok(());
        }
        return Err(error);
    }
    println!(
        "Polling {} PIDs every {} seconds; stop with Ctrl+C.",
        pids::PIDS.len(),
        POLL_INTERVAL.as_secs()
    );
    println!("Mazda-specific PID formulas have not been verified against this vehicle.");

    while running.load(Ordering::SeqCst) {
        let start = Instant::now();
        let mut healthy = true;
        for pid in pids::PIDS {
            if !running.load(Ordering::SeqCst) {
                break;
            }
            // A late reply belongs to the previous request, never to the next PID.
            if !adapter.synchronized() {
                healthy = false;
                leds.set(LedState::Problem);
                let late = adapter.receive(adapter::RESPONSE_TIMEOUT, running);
                event(
                    &mut errors,
                    "resync",
                    late.error
                        .as_deref()
                        .unwrap_or("Recovered adapter prompt; discarded late response"),
                    &late.raw,
                )?;
            }
            if !running.load(Ordering::SeqCst) {
                break;
            }
            let reply = adapter.query(pid.command, running);
            #[cfg(feature = "rtc")]
            let time = timestamp();
            #[cfg(not(feature = "rtc"))]
            let time = {
                let elapsed = session_start.elapsed();
                format!("{}.{:03}", elapsed.as_secs(), elapsed.subsec_millis())
            };
            let result = match reply.error {
                Some(error) => Err(error),
                None => protocol::payload(&reply.raw, pid.command)
                    .and_then(|payload| pids::measurements(pid, &payload)),
            };
            match result {
                Ok(values) => {
                    for (name, value) in values {
                        // f64::to_string keeps the small fractional values of Mazda PIDs.
                        let value = value.to_string();
                        csv.write_record([&time, pid.command, name, &reply.raw, &value, pid.unit])?;
                        println!("{time} {} {name} = {value} {}", pid.command, pid.unit);
                    }
                }
                Err(error) => {
                    healthy = false;
                    leds.set(LedState::Problem);
                    csv.write_record([&time, pid.command, pid.name, &reply.raw, "", pid.unit])?;
                    event(&mut errors, pid.command, &error, &reply.raw)?;
                    eprintln!("{time} {} {}: {error}", pid.command, pid.name);
                }
            }
            // Persist every query, including errors and partially received replies.
            csv.flush()?;
        }
        if !running.load(Ordering::SeqCst) {
            break;
        }
        if start.elapsed() > POLL_INTERVAL {
            healthy = false;
            let message = format!(
                "Polling sweep took {:.2}s; target is {:.2}s. Next sweep starts now.",
                start.elapsed().as_secs_f64(),
                POLL_INTERVAL.as_secs_f64()
            );
            event(&mut errors, "schedule", &message, "")?;
            eprintln!("{message}");
        }
        // Only report healthy after a complete sweep has been written and flushed.
        leds.set(if healthy {
            LedState::Recording
        } else {
            LedState::Problem
        });
        while running.load(Ordering::SeqCst) && start.elapsed() < POLL_INTERVAL {
            std::thread::sleep(
                POLL_INTERVAL
                    .saturating_sub(start.elapsed())
                    .min(Duration::from_millis(100)),
            );
        }
    }
    csv.flush()?;
    event(&mut errors, "logger", "Stopped", "")?;
    println!("Stopped. CSV flushed.");
    Ok(())
}

fn main() -> std::process::ExitCode {
    let result = (|| -> Result<(), Box<dyn Error>> {
        let Some(options) = options()? else {
            return Ok(());
        };
        let running = Arc::new(AtomicBool::new(true));
        let signal_flag = Arc::clone(&running);
        let mut leds = StatusLeds::new(options.leds)?;
        leds.set(LedState::Problem);
        let result = (|| {
            ctrlc::set_handler(move || signal_flag.store(false, Ordering::SeqCst))?;
            run(options, &running, &mut leds)
        })();
        leds.finish(result.is_err());
        result
    })();
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
