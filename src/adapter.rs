use serialport::{ClearBuffer, SerialPort};
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(2);
const READ_TIMEOUT: Duration = Duration::from_millis(100);
const MAX_RESPONSE_BYTES: usize = 65536;

pub struct Reply {
    pub raw: String,
    pub error: Option<String>,
}

pub struct Adapter {
    port: Box<dyn SerialPort>,
    synchronized: bool,
}

// Preserve ASCII (including CR/LF/prompt) exactly. Unexpected bytes are represented
// as Latin-1 code points, so every received byte remains recoverable from the CSV.
fn raw_text(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

impl Adapter {
    pub fn open(path: &str, baud: u32) -> Result<Self, Box<dyn std::error::Error>> {
        let port = serialport::new(path, baud)
            .timeout(READ_TIMEOUT)
            .flow_control(serialport::FlowControl::None)
            .open()?;
        port.clear(ClearBuffer::Input)?;
        Ok(Self {
            port,
            synchronized: true,
        })
    }

    pub fn synchronized(&self) -> bool {
        self.synchronized
    }

    pub fn receive(&mut self, timeout: Duration, running: &AtomicBool) -> Reply {
        let deadline = Instant::now() + timeout;
        let mut raw = Vec::new();
        self.synchronized = false;
        let error = loop {
            if !running.load(Ordering::SeqCst) {
                break "Interrupted while waiting for adapter prompt".to_owned();
            }
            if Instant::now() >= deadline {
                break "Timed out waiting for adapter prompt".to_owned();
            }
            // Reading one byte prevents consuming bytes after the prompt.
            let mut byte = [0];
            match self.port.read(&mut byte) {
                Ok(0) => break "Serial port returned EOF (adapter disconnected)".into(),
                Ok(_) => {
                    raw.push(byte[0]);
                    if byte[0] == b'>' {
                        self.synchronized = true;
                        return Reply {
                            raw: raw_text(&raw),
                            error: None,
                        };
                    }
                    if raw.len() >= MAX_RESPONSE_BYTES {
                        break "Adapter response exceeded 65536 bytes; preserving this chunk"
                            .into();
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::TimedOut
                            | io::ErrorKind::WouldBlock
                            | io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => break format!("Serial read failed: {e}"),
            }
        };
        Reply {
            raw: raw_text(&raw),
            error: Some(error),
        }
    }

    fn send(&mut self, command: &str, running: &AtomicBool) -> Reply {
        if !running.load(Ordering::SeqCst) {
            return Reply {
                raw: String::new(),
                error: Some("Interrupted before request".into()),
            };
        }
        if !self.synchronized {
            return Reply {
                raw: String::new(),
                error: Some("Adapter has no prompt; request not sent".into()),
            };
        }
        self.synchronized = false;
        if let Err(e) = self.port.write_all(format!("{command}\r").as_bytes()) {
            return Reply {
                raw: String::new(),
                error: Some(format!("Serial write failed: {e}")),
            };
        }
        self.receive(RESPONSE_TIMEOUT, running)
    }

    pub fn query(&mut self, command: &str, running: &AtomicBool) -> Reply {
        // No raw-command CLI or arbitrary services can reach the serial port.
        let allowed = crate::protocol::hex_bytes(command).is_ok_and(|bytes| {
            matches!(
                (bytes.first(), bytes.len()),
                (Some(0x01), 2) | (Some(0x22), 3)
            )
        });
        if !allowed {
            return Reply {
                raw: String::new(),
                error: Some("Only service 01 PID and service 22 DID requests are allowed".into()),
            };
        }
        self.send(command, running)
    }

    pub fn initialize(
        &mut self,
        running: &AtomicBool,
        mut record: impl FnMut(&str, &Reply) -> io::Result<()>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Adapter configuration only; no ECU configuration or diagnostic sessions.
        for command in [
            "ATZ", "ATE0", "ATL0", "ATS1", "ATH1", "ATD0", "ATSP6", "ATCAF1", "ATCFC1", "ATFCSM0",
            "ATST64", "ATAT1", "ATSH7E0", "ATCRA7E8",
        ] {
            let reply = self.send(command, running);
            record(command, &reply)?;
            if let Some(error) = reply.error {
                return Err(format!("Adapter initialization {command} failed: {error}").into());
            }
            let lines: Vec<_> = reply
                .raw
                .split(['\r', '\n', '>'])
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.eq_ignore_ascii_case(command))
                .collect();
            let valid = if command == "ATZ" {
                lines
                    .iter()
                    .any(|l| l.contains("ELM327") || l.contains("STN") || l.contains("vLinker"))
            } else {
                lines == ["OK"]
            };
            if !valid {
                return Err(format!("Adapter rejected {command}: {:?}", reply.raw).into());
            }
        }
        Ok(())
    }
}
