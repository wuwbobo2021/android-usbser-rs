// This is merely a simplest test program for the library crate.
// To build a regular application, please use some UI framework like Slint (or Tauri?).

use android_activity::{AndroidApp, MainEvent, PollEvent};
use android_usbser::{CdcSerial, SerialConfig};
use log::{info, warn};
use serialport::SerialPort;
use std::{
    io::{self, BufRead, Write},
    str::FromStr,
    sync::Mutex,
    time::{Duration, SystemTime},
};

#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info) // this can be set to `Debug`
            .with_tag("android_usb_cdc_test"),
    );

    let mut on_destroy = false;
    loop {
        app.poll_events(
            Some(std::time::Duration::from_secs(1)), // timeout
            |event| match event {
                PollEvent::Main(MainEvent::Start) => {
                    info!("Main Start.");
                    start_serial_thread();
                }
                PollEvent::Main(MainEvent::Stop) => {
                    info!("Main Stop.");
                    stop_serial_thread();
                }
                PollEvent::Main(MainEvent::Destroy) => {
                    info!("Main Destroy.");
                    on_destroy = true;
                }
                _ => (),
            },
        );
        if on_destroy {
            return;
        }
    }
}

static SERIAL_THREAD: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);
static FLAG_EXIT: Mutex<bool> = Mutex::new(false);

fn start_serial_thread() {
    let mut th_hdl = SERIAL_THREAD.lock().unwrap();
    if th_hdl.is_none() {
        th_hdl.replace(std::thread::spawn(serial_probe_thread));
        info!("Serial thread started.");
    }
}

fn stop_serial_thread() {
    let thread_exists = SERIAL_THREAD.lock().unwrap().is_some();
    if thread_exists {
        *FLAG_EXIT.lock().unwrap() = true;
    }
}

// Functions below are executed in the serial thread.

#[inline(always)]
fn check_flag_exit() -> bool {
    let mut flag_exit = FLAG_EXIT.lock().unwrap();
    if *flag_exit {
        *flag_exit = false;
        true
    } else {
        false
    }
}

fn thread_delay_ms(ms: u64) -> bool {
    let t_break = SystemTime::now() + Duration::from_millis(ms);
    while SystemTime::now() < t_break {
        if check_flag_exit() {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}

fn serial_probe_thread() {
    serial_probe_loop();
    let _ = SERIAL_THREAD.lock().unwrap().take().unwrap();
    info!("Serial thread exits normally.");
}

fn serial_probe_loop() {
    loop {
        if !thread_delay_ms(1000) {
            return;
        }

        let usb_cdc_dev = {
            let usb_cdc_devs = CdcSerial::probe().unwrap();
            if usb_cdc_devs.is_empty() {
                info!("No CDC serial adapter found.");
                if !thread_delay_ms(1000) {
                    return;
                }
                continue;
            }
            usb_cdc_devs.into_iter().next().unwrap()
        };

        info!("{usb_cdc_dev:#?}");
        info!("Opening {:?} ...", usb_cdc_dev.id());

        let mut serial = match CdcSerial::build(&usb_cdc_dev, Duration::from_millis(300)) {
            Ok(serial) => serial,
            Err(e) => {
                info!("Failed to connect: {e}");
                continue;
            }
        };
        let initial_conf = "115200,N,8,1".parse().unwrap();
        info!("Opened, setting {initial_conf} ...");
        serial.set_config(initial_conf).unwrap();
        info!("Configuration set.");

        serial_conn_loop(serial);

        if !thread_delay_ms(1000) {
            return;
        }
    }
}

fn serial_conn_loop(mut serial: CdcSerial) {
    let mut last_cmd = String::new();
    loop {
        if check_flag_exit() {
            return;
        }

        last_cmd.clear();
        match serial.read_line(&mut last_cmd) {
            Ok(sz) => info!("{sz} bytes read with `BufRead::read_line`."),
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
                    warn!("Error: 'conf' without parameter.");
                    continue;
                }
                if let Ok(conf) = SerialConfig::from_str(conf) {
                    if let Err(s) = serial.set_config(conf) {
                        warn!("{s}");
                        continue;
                    }
                } else {
                    warn!("Error: failed to parse '{conf}' into serial parameters.");
                    continue;
                }
            }
            "rts" => {
                let value = match iter_tokens.next().map(|s| s.trim()) {
                    Some("0") | Some("false") => false,
                    Some("1") | Some("true") => true,
                    _ => {
                        warn!("Error: failed to parse RTS value.");
                        continue;
                    }
                };
                if let Err(e) = serial.write_request_to_send(value) {
                    warn!("Error: failed to write RTS value: {e}");
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
            warn!("Error: failed to response: {e}");
        }
    }
}
