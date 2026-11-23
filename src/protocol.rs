pub fn hex_bytes(text: &str) -> Result<Vec<u8>, String> {
    if text.is_empty()
        || !text.is_ascii()
        || !text.len().is_multiple_of(2)
        || !text.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(format!("Invalid hexadecimal data: {text:?}"));
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

// ATH1 + ATS1 + ATD0: 11-bit CAN ID followed by ISO-TP PCI and data.
// Reject other formats instead of guessing where payload bytes begin.
pub fn payload(raw: &str, command: &str) -> Result<Vec<u8>, String> {
    let mut messages = Vec::new();
    let mut assembled = Vec::new();
    let mut expected = 0;
    let mut sequence = 1;
    for line in raw
        .split(['\r', '\n', '>'])
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        if line.eq_ignore_ascii_case(command) {
            continue;
        }
        let mut fields = line.split_ascii_whitespace();
        let id = fields.next().unwrap();
        if !id.eq_ignore_ascii_case("7E8") {
            return Err(format!("Unexpected CAN header or adapter status: {line:?}"));
        }
        let frame: Vec<u8> = fields
            .map(|byte| {
                if byte.len() != 2 {
                    return Err(format!("Invalid CAN byte: {byte:?}"));
                }
                let bytes = hex_bytes(byte)?;
                Ok(bytes[0])
            })
            .collect::<Result<_, String>>()?;
        if frame.is_empty() || frame.len() > 8 {
            return Err(format!("Invalid CAN frame length: {}", frame.len()));
        }
        match frame[0] >> 4 {
            0 => {
                let length = frame[0] as usize;
                if expected != 0 || length == 0 || length > 7 || frame.len() < length + 1 {
                    return Err("Invalid or interleaved ISO-TP single frame".into());
                }
                messages.push(frame[1..=length].to_vec());
            }
            1 => {
                if expected != 0 || frame.len() != 8 {
                    return Err("Invalid or interleaved ISO-TP first frame".into());
                }
                expected = (((frame[0] & 0x0f) as usize) << 8) | frame[1] as usize;
                if expected <= 7 {
                    return Err("Invalid ISO-TP multi-frame length".into());
                }
                assembled = frame[2..].to_vec();
                sequence = 1;
            }
            2 => {
                if expected == 0 || frame[0] & 0x0f != sequence || frame.len() < 2 {
                    return Err("Unexpected ISO-TP consecutive frame or sequence number".into());
                }
                let remaining = expected - assembled.len();
                if frame.len() - 1 < remaining.min(7) {
                    return Err("Truncated ISO-TP consecutive frame".into());
                }
                assembled.extend_from_slice(&frame[1..1 + remaining.min(frame.len() - 1)]);
                sequence = (sequence + 1) & 0x0f;
                if assembled.len() == expected {
                    messages.push(std::mem::take(&mut assembled));
                    expected = 0;
                }
            }
            _ => return Err("Unexpected ISO-TP frame type".into()),
        }
    }
    if expected != 0 {
        return Err(format!(
            "Incomplete ISO-TP response: {} of {expected} bytes",
            assembled.len()
        ));
    }
    // A response-pending NRC is not a measurement. Only accept it before a final response.
    let service = hex_bytes(command)?[0];
    while messages.len() > 1 && messages[0] == [0x7f, service, 0x78] {
        messages.remove(0);
    }
    if messages.len() != 1 {
        return Err(format!(
            "Expected one ECU response, received {}",
            messages.len()
        ));
    }
    Ok(messages.remove(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pids::{measurements, PIDS};

    #[test]
    fn validates_headers_lengths_and_sequences() {
        assert_eq!(
            payload("010C\r7E8 04 41 0C 1F 40 00 00 00\r>", "010C").unwrap(),
            [0x41, 0x0c, 0x1f, 0x40]
        );
        for raw in [
            "NO DATA\r>",
            "7E9 03 41 05 50\r>",
            "41 05 50\r>",
            "7E8 04 41 0C 1F\r>",
            "7E8 10 0B 41 78 01 03 E8 00\r>",
            "7E8 10 0B 41 78 01 03 E8 00\r7E8 22 00 00 00 00 00\r>",
            "7E8 03 41 05 50\rCAN ERROR\r>",
        ] {
            assert!(payload(raw, "010C").is_err(), "{raw}");
        }
        assert!(measurements(&PIDS[0], &[0x62, 0xdd, 2, 1, 2, 3]).is_err());
        assert!(measurements(&PIDS[0], &[0x62, 0xdd, 1, 1, 2]).is_err());
        assert!(measurements(&PIDS[8], &[0x7f, 1, 0x12]).is_err());
    }

    #[test]
    fn decodes_community_formulas_and_standard_values() {
        let samples: &[(&[u8], f64)] = &[
            (&[1, 2, 3], 66051.0),
            (&[2], 1.0),
            (&[0xff, 1, 0, 0], 100.0),
            (&[1, 2], 258.0),
            (&[0x80, 0], 50.0),
            (&[0x40, 0], 25.0),
            (&[0x20, 0], 12.5),
            (&[0x99, 0xff, 0x9c], -1.0),
            (&[0x1f, 0x40], 2000.0),
            (&[88], 88.0),
            (&[130], 90.0),
            (&[1, 244], 5.0),
            (&[0x36, 0xb0], 14.0),
        ];
        for (pid, (data, expected)) in PIDS.iter().zip(samples) {
            let mut response = hex_bytes(pid.command).unwrap();
            response[0] += 0x40;
            response.extend_from_slice(data);
            assert_eq!(
                measurements(pid, &response).unwrap()[0].1,
                *expected,
                "{}",
                pid.command
            );
        }
    }

    #[test]
    fn decodes_multiframe_egt_support_bitmap() {
        let raw = "7E8 10 0B 41 78 05 03 E8 00\r7E8 21 00 07 D0 00 00 00 00\r>";
        let data = payload(raw, "0178").unwrap();
        let values = measurements(&PIDS[13], &data).unwrap();
        assert_eq!(
            values,
            [
                ("exhaust_temperature_bank1_sensor1", 60.0),
                ("exhaust_temperature_bank1_sensor3", 160.0)
            ]
        );
        assert!(measurements(&PIDS[13], &[0x41, 0x78, 1, 0, 0]).is_err());
    }
}
