use android_usbser::{CdcSerial, SerialConfig};
use serialport::SerialPort;
use std::{
    io::{self, BufRead, Write},
    str::FromStr,
    time::Duration,
};

fn main() {
    loop {
        let usb_cdc_dev = {
            let usb_cdc_devs = CdcSerial::probe().unwrap();
            if usb_cdc_devs.is_empty() {
                println!("No CDC serial adapter found.");
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
            usb_cdc_devs.into_iter().next().unwrap()
        };

        println!("Opening CDC device ...");
        let mut serial = CdcSerial::build(&usb_cdc_dev, Duration::from_millis(300)).unwrap();
        let initial_conf = "115200,N,8,1".parse().unwrap();
        println!("Opened, setting {initial_conf} ...");
        serial.set_config(initial_conf).unwrap();
        println!("Configuration set.");

        serial_conn_loop(serial);

        std::thread::sleep(Duration::from_secs(1));
    }
}

fn serial_conn_loop(mut serial: CdcSerial) {
    let mut last_cmd = String::new();
    loop {
        last_cmd.clear();
        match serial.read_line(&mut last_cmd) {
            Ok(sz) => println!("{sz} bytes read with `BufRead::read_line`."),
            Err(e) if e.kind() == io::ErrorKind::NotConnected => return,
            _ => continue,
        }

        let mut iter_tokens = last_cmd.split_whitespace();
        let Some(cmd_name) = iter_tokens.next() else {
            continue;
        };

        let mut is_cmd = true;
        match cmd_name {
            "conf" => {
                let conf;
                if let Some(s) = iter_tokens.next() {
                    conf = s.trim();
                } else {
                    println!("Error: 'conf' without parameter.");
                    continue;
                }
                if let Ok(conf) = SerialConfig::from_str(conf) {
                    if let Err(s) = config_serialport(&mut serial, &conf) {
                        println!("{s}");
                        continue;
                    }
                } else {
                    println!("Error: failed to parse '{conf}' into serial parameters.");
                    continue;
                }
            }
            "rts" => {
                let value = match iter_tokens.next().map(|s| s.trim()) {
                    Some("0") | Some("false") => false,
                    Some("1") | Some("true") => true,
                    _ => {
                        println!("Error: failed to parse RTS value.");
                        continue;
                    }
                };
                if let Err(e) = serial.write_request_to_send(value) {
                    println!("Error: failed to write RTS value: {e}");
                    continue;
                }
            }
            _ => {
                is_cmd = false; // loopback
            }
        }

        let result = if is_cmd {
            serial.write_all("Ok\n".as_bytes())
        } else {
            let last_line_upper = last_cmd.to_uppercase();
            serial.write_all(last_line_upper.as_bytes())
        }
        .and_then(|_| serial.flush());
        if let Err(e) = result {
            println!("Error: failed to response: {e}");
        }
    }
}

// `set_config()` is available in `CdcSerial`, but this is testing `SerialPort` trait impl.
fn config_serialport(serial: &mut dyn SerialPort, conf: &SerialConfig) -> Result<(), String> {
    serial
        .set_baud_rate(conf.baud_rate)
        .map_err(|e| format!("Error: failed to set baudrate: {e}."))?;
    serial
        .set_parity(conf.parity)
        .map_err(|e| format!("Error: failed to set parity: {e}."))?;
    serial
        .set_data_bits(conf.data_bits)
        .map_err(|e| format!("Error: failed to set data bits: {e}."))?;
    serial
        .set_stop_bits(conf.stop_bits)
        .map_err(|e| format!("Error: failed to set stop bits: {e}."))?;

    println!(
        "SerialPort parameters set: {} {} {} {}",
        conf.baud_rate, conf.parity, conf.data_bits, conf.stop_bits
    );
    Ok(())
}
