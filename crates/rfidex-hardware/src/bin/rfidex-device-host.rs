//! Isolated SDK child and explicit developer-only disposable-tag commissioning.

use rfidex_hardware::{HardwareClient, HardwareConfig, HostLauncher, Operation, Response};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
}

fn parse_hex(text: &str) -> Result<Vec<u8>, &'static str> {
    if text.is_empty() || !text.len().is_multiple_of(2) {
        return Err("hex data must contain complete bytes");
    }
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let text = std::str::from_utf8(pair).map_err(|_| "invalid hex")?;
            u8::from_str_radix(text, 16).map_err(|_| "invalid hex")
        })
        .collect()
}

fn commissioning(args: &[String]) -> Result<(), String> {
    let [config_path, operation, rest @ ..] = args else {
        return Err("usage: rfidex-device-host commissioning <config.json> records | write --disposable-tag <UID-hex-16> <start-block> <data-hex>".into());
    };
    let config: HardwareConfig =
        serde_json::from_slice(&std::fs::read(config_path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    if !matches!(config, HardwareConfig::EcrfidSdk { .. }) {
        return Err("commissioning requires ecrfid_sdk config".into());
    }

    // Parse and validate all write parameters before opening a native reader.
    let write = match (operation.as_str(), rest) {
        ("records", []) => None,
        ("write", [flag, uid, start, data]) if flag == "--disposable-tag" => {
            let uid = parse_hex(uid).map_err(str::to_string)?;
            let uid: [u8; 8] = uid.try_into().map_err(|_| "UID must be exactly 8 bytes")?;
            let start = start.parse::<u8>().map_err(|_| "invalid start block")?;
            let data = parse_hex(data).map_err(str::to_string)?;
            if data.len() < 4 || !data.len().is_multiple_of(4) || data.len() > 32 {
                return Err("data must be 1..8 exact four-byte blocks".into());
            }
            Some((uid, start, data))
        }
        _ => {
            return Err(
                "use records or write --disposable-tag <UID-hex-16> <start-block> <data-hex>"
                    .into(),
            )
        }
    };

    let launcher = HostLauncher::current_exe().map_err(|error| error.to_string())?;
    let mut client = HardwareClient::start_commissioning(&launcher, &config)
        .map_err(|error| error.to_string())?;
    let result = (|| {
        if let Some((uid, start, data)) = write {
            let count = (data.len() / 4) as u8;
            // Baseline first. A torn write must never trigger an automatic retry.
            let baseline = match client
                .call(Operation::Read { uid, start, count })
                .map_err(|error| error.to_string())?
            {
                Response::Bytes { data } => data,
                _ => return Err("unexpected baseline response".into()),
            };
            match client
                .call(Operation::Write {
                    uid,
                    start,
                    data: data.clone(),
                })
                .map_err(|error| error.to_string())?
            {
                Response::Unit => (),
                _ => return Err("unexpected write response".into()),
            }
            let readback = match client
                .call(Operation::Read { uid, start, count })
                .map_err(|error| error.to_string())?
            {
                Response::Bytes { data } => data,
                _ => return Err("unexpected readback response".into()),
            };
            if readback != data {
                return Err(format!(
                    "readback mismatch; baseline={} observed={}; do not retry this tag",
                    hex(&baseline),
                    hex(&readback)
                ));
            }
            println!(
                "verified UID={} start={} baseline={} readback={}",
                hex(&uid),
                start,
                hex(&baseline),
                hex(&readback)
            );
            Ok(())
        } else {
            match client
                .call(Operation::RawRecords)
                .map_err(|error| error.to_string())?
            {
                Response::Records { raw } => {
                    for record in raw {
                        println!("{}", hex(&record));
                    }
                    Ok(())
                }
                _ => Err("unexpected records response".into()),
            }
        }
    })();
    if client.is_alive() {
        let close = client
            .call(Operation::Close)
            .map_err(|error| error.to_string());
        if result.is_ok() && close != Ok(Response::Unit) {
            return Err("reader close failed".into());
        }
    }
    result
}

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if let Some(code) = rfidex_hardware::run_host_from_args(&args) {
        std::process::exit(code);
    }
    let args: Vec<String> = args
        .into_iter()
        .map(|value| value.into_string())
        .collect::<Result<_, _>>()
        .unwrap_or_default();
    let [command, rest @ ..] = args.as_slice() else {
        eprintln!("use commissioning <config.json> records | write --disposable-tag <UID-hex-16> <start-block> <data-hex>");
        std::process::exit(2);
    };
    if command != "commissioning" {
        eprintln!("only explicit commissioning command is supported");
        std::process::exit(2);
    }
    if let Err(error) = commissioning(rest) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
