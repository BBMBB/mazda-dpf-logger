# mazda-dpf-logger

Read-only OBD logger for a Mazda CX-5 KE, first registered 08/2016,
2.2 SKYACTIV-D AWD automatic. Uses a Vgate vLinker FS USB on Linux and writes
one CSV file per run.

Reads mileage, regeneration status and counters, soot load and DPF differential
pressure, plus RPM, speed, coolant temperature, mass air flow, ECU voltage and
bank 1 exhaust temperatures where supported. Only services `01` and `22` are
allowed. It cannot write ECU parameters, clear faults, trigger regeneration or
run active tests. AT commands configure the adapter.

The Mazda PIDs and formulas come from community research and have not been
verified against this specific vehicle. Check decoded values against a trusted
diagnostic tool. Operation with the vehicle, adapter and Pi GPIO hardware has
not yet been validated.

## Build and run

Requires stable Rust with Cargo, Linux build tools and a C linker. The USB
serial driver must be loaded and the user must have access to the port, often
through the `dialout` group. No `libudev` development package is needed.

```sh
cargo build --release
./target/release/mazda-dpf-logger /dev/ttyUSB0 --baud 115200 --output logs
```

Turn the ignition on and close other applications using the adapter.
Prefer `/dev/serial/by-id/...` for a persistent device path. `--baud` sets the
host serial rate, which must match the adapter; the default is 115200.
`--output` defaults to `logs` in the working directory. Use `--help` for CLI
options and Ctrl+C to stop.

## Logs

Each run creates `drive_<UTC timestamp>.csv` and a matching `.errors.log` without
overwriting existing files. The error log records adapter initialization,
failed queries, late responses and polling overruns. Query failures do not stop
logging; port setup and file errors do.

```text
timestamp,pid,name,raw_hex,value,unit
```

Each CSV record contains one measurement, timestamped at response completion in
UTC (RFC 3339). `raw_hex` keeps the original adapter response, including CAN
headers, padding, whitespace, line breaks, any echo and the `>` prompt. Partial
responses are kept too. Use a CSV reader: a record can span several lines.
Unexpected non-ASCII bytes are stored as their Latin-1 code points in UTF-8,
so their original byte values can be recovered.

Missing, negative, unsupported or invalid responses leave `value` empty; the
reason goes in the error log. PID `0178` produces a record for each supported
bank 1 EGT sensor, up to four, sharing the same raw response. An empty support
bitmap produces a record with no value. Sensor locations are not inferred.
Late responses after a timeout go in the error log, never under the next PID.

Logs are flushed after every query, without `fsync`. This does not guarantee
data survives a sudden power loss. CSV and log files are ignored by Git;
do not commit vehicle identifiers.

## Configuration

Configuration is in the source:

- `src/main.rs`: `POLL_INTERVAL`, five seconds between polling round starts.
- `src/pids.rs`: PIDs, names, units, minimum data lengths and formulas.
- `src/adapter.rs`: initialization and response timeout (two seconds).

Every configured PID is queried sequentially, including those returning
`NO DATA`. If a round exceeds five seconds, the overrun is logged and the next
round starts immediately. There are no catch-up bursts. Serial reads time out
after 100 ms so Ctrl+C can interrupt a pending response.

CAN is fixed to ISO 15765-4, 11-bit / 500 kbit/s (`ATSP6`), with requests to
`7E0` and responses from `7E8`. Headers and spaces are on, DLC display is off.
The adapter handles ISO-TP formatting and flow control. There is no protocol
or ECU scan.

## Raspberry Pi 4

For GPIO status LEDs, build on the Pi with `pi-leds`:

```sh
cargo build --release --features pi-leds
./target/release/mazda-dpf-logger /dev/ttyUSB0 --output /mnt/obd-data --leds
```

For a read-only root filesystem, build and install first, then mount the data
partition read/write before starting the logger. All CSV and error log writes
go under `--output`. Use an absolute path. Missing directories are created;
an unwritable path stops the program without falling back to another location.
The logger does not mount partitions or check whether the path is a mount point.
Console logging managed by the OS is separate.

The default build excludes `rppal`. With `pi-leds` compiled in, GPIO is only
accessed when `--leds` is passed. Using `--leds` without the feature, or failing
to initialize GPIO, stops startup. The user needs access to `/dev/gpiomem`,
usually through the `gpio` group. No GPIO sysfs exports are used.

Connect active-high LEDs from GPIO17 (physical pin 11, green) and GPIO27
(physical pin 13, red), each through a series resistor and LED to GND.

- Green: a complete polling round succeeded and its records were flushed.
- Red: startup, a failed query, resynchronization, an overrun, or a serial setup
  or log write error. Recovery requires a full successful round. Unsupported
  configured PIDs count as errors too, including `0178`; remove them from
  `src/pids.rs` if needed.
- Both off: clean shutdown. Fatal errors leave red on until the GPIO is
  reconfigured or the Pi resets.

LED indications do not cover power loss, SIGKILL, a stalled process or GPIO
initialization failure.

## Limitations

Only the configured engine ECU is queried. PID support depends on its firmware.
The parser checks CAN ID, ISO-TP lengths and sequence numbers, response service,
PID/DID and data length before applying byte offsets. Standard PIDs require
exact data lengths. Mazda PIDs accept the minimum length needed by the formula
because their full layouts are unverified; extra bytes remain in the raw response.
The parser rejects unfamiliar response formats.

After a missing prompt, no further command is sent until the prompt returns.
An unplugged or permanently unresponsive adapter requires a restart; there is
no automatic reconnection. OS read-only setup, autostart and a GUI are outside
the project.

Protocol references: [ELM327 datasheet](https://www.elmelectronics.com/wp-content/uploads/2016/07/ELM327DSH.pdf)
and [SAE J1979-DA, table B100 for PID 78](https://www.sae.org/standards/content/j1979da_201110/).

## License

MIT; see [LICENSE](LICENSE).

Unofficial project. Not affiliated with or endorsed by Mazda Motor Corporation.
