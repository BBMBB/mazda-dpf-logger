pub struct Pid {
    pub command: &'static str,
    pub name: &'static str,
    pub unit: &'static str,
    pub length: usize,
    pub decode: fn(&[u8]) -> f64,
}

fn word(data: &[u8]) -> f64 {
    u16::from_be_bytes([data[0], data[1]]) as f64
}

// A is the first byte AFTER the positive response service and PID/DID.
// Mazda lengths are minimum lengths: extra ECU-specific bytes remain in raw_hex.
pub const PIDS: &[Pid] = &[
    Pid {
        command: "22DD01",
        name: "odometer",
        unit: "km",
        length: 3,
        decode: |d| u32::from_be_bytes([0, d[0], d[1], d[2]]) as f64,
    },
    Pid {
        command: "220380",
        name: "regeneration_active",
        unit: "0/1",
        length: 1,
        decode: |d| ((d[0] >> 1) & 1) as f64,
    },
    Pid {
        command: "220434",
        name: "distance_since_regeneration",
        unit: "km",
        length: 4,
        decode: |d| u32::from_be_bytes([0, d[1], d[2], d[3]]) as f64 * 100.0 / 65536.0,
    },
    Pid {
        command: "220432",
        name: "regeneration_count",
        unit: "count",
        length: 2,
        decode: word,
    },
    Pid {
        command: "22042C",
        name: "pm_acc",
        unit: "g/l",
        length: 2,
        decode: |d| word(d) * 100.0 / 65536.0,
    },
    Pid {
        command: "22042B",
        name: "pm_acc_dsd",
        unit: "g/l",
        length: 2,
        decode: |d| word(d) * 100.0 / 65536.0,
    },
    Pid {
        command: "22042D",
        name: "pm_gen",
        unit: "g/l",
        length: 2,
        decode: |d| word(d) * 100.0 / 65536.0,
    },
    Pid {
        command: "22F47A",
        name: "dpf_differential_pressure",
        unit: "kPa",
        length: 3,
        decode: |d| i16::from_be_bytes([d[1], d[2]]) as f64 / 100.0,
    },
    Pid {
        command: "010C",
        name: "engine_speed",
        unit: "rpm",
        length: 2,
        decode: |d| word(d) / 4.0,
    },
    Pid {
        command: "010D",
        name: "vehicle_speed",
        unit: "km/h",
        length: 1,
        decode: |d| d[0] as f64,
    },
    Pid {
        command: "0105",
        name: "coolant_temperature",
        unit: "degC",
        length: 1,
        decode: |d| d[0] as f64 - 40.0,
    },
    Pid {
        command: "0110",
        name: "mass_air_flow",
        unit: "g/s",
        length: 2,
        decode: |d| word(d) / 100.0,
    },
    Pid {
        command: "0142",
        name: "ecu_voltage",
        unit: "V",
        length: 2,
        decode: |d| word(d) / 1000.0,
    },
    Pid {
        command: "0178",
        name: "exhaust_temperature_bank1",
        unit: "degC",
        length: 9,
        decode: |_| unreachable!("EGT has four separate measurements"),
    },
];

pub const EGT_NAMES: [&str; 4] = [
    "exhaust_temperature_bank1_sensor1",
    "exhaust_temperature_bank1_sensor2",
    "exhaust_temperature_bank1_sensor3",
    "exhaust_temperature_bank1_sensor4",
];

pub fn measurements(pid: &Pid, payload: &[u8]) -> Result<Vec<(&'static str, f64)>, String> {
    let request = crate::protocol::hex_bytes(pid.command)?;
    if !matches!(
        (request.first(), request.len()),
        (Some(0x01), 2) | (Some(0x22), 3)
    ) {
        return Err("Only service 01 PID and service 22 DID requests are allowed".into());
    }
    if payload.starts_with(&[0x7f, request[0]]) {
        return Err(format!("Negative response: {payload:02X?}"));
    }
    let mut header = request;
    header[0] += 0x40;
    if !payload.starts_with(&header) {
        return Err(format!(
            "Expected response header {header:02X?}, received {payload:02X?}"
        ));
    }
    let data = &payload[header.len()..];
    if data.len() < pid.length || (pid.command.starts_with("01") && data.len() != pid.length) {
        return Err(format!(
            "Invalid data length: expected {}{} bytes, received {}",
            if pid.command.starts_with("22") {
                "at least "
            } else {
                ""
            },
            pid.length,
            data.len()
        ));
    }
    if pid.command != "0178" {
        return Ok(vec![(pid.name, (pid.decode)(data))]);
    }
    if data[0] & 0xf0 != 0 {
        return Err("EGT support bitmap contains reserved bits".into());
    }
    let values: Vec<_> = EGT_NAMES
        .iter()
        .enumerate()
        .filter(|(sensor, _)| data[0] & (1 << sensor) != 0)
        .map(|(sensor, name)| (*name, word(&data[1 + sensor * 2..]) / 10.0 - 40.0))
        .collect();
    if values.is_empty() {
        return Err("No EGT sensors supported by the response bitmap".into());
    }
    Ok(values)
}
