use std::{
    io::{self, BufRead, Error, ErrorKind, Read, Write},
    time::Duration,
};

use crate::{SerialConfig, UsbSerial};

#[cfg(target_os = "android")]
use crate::usb::{self, DeviceInfo, InterfaceInfo};

#[cfg(not(target_os = "android"))]
use nusb::{self as usb, DeviceInfo, InterfaceInfo};

use nusb::transfer::{Bulk, ControlOut, ControlType, Direction, In, Out, Recipient};
use nusb::{
    io::{EndpointRead, EndpointWrite},
    MaybeFuture,
};

use serialport::{DataBits, Parity, SerialPort, StopBits};

const USB_INTR_CLASS_COMM: u8 = 0x02;
const USB_INTR_SUBCLASS_ACM: u8 = 0x02;
const USB_INTR_CLASS_CDC_DATA: u8 = 0x0A;

const SET_LINE_CODING: u8 = 0x20;
const SET_CONTROL_LINE_STATE: u8 = 0x22;
const SEND_BREAK: u8 = 0x23;

/// This is currently a thin wrapper of USB operations, it requires hardware buffers
/// at the device side. It uses the CDC ACM Data Interface Class to transfer data
/// (the Communication Interface Class is used for probing and serial configuration).
///
/// Reference: *USB Class Definitions for Communication Devices, Version 1.1*,
/// especially section 3.6.2.1, 5.2.3.2 and 6.2(.13).
pub struct CdcSerial {
    usb_path_name: String,      // the name from `android.hardware.usb.UsbDevice`
    ctrl_index: u16,            // communication interface id as the control transfer index
    intr_comm: nusb::Interface, // communication interface keeper
    reader: EndpointRead<Bulk>,
    writer: EndpointWrite<Bulk>,

    timeout: Duration,              // standard `Read` and `Write` timeout
    ser_conf: Option<SerialConfig>, // keeps the latest settings
    dtr_rts: (bool, bool),          // keeps the latest settings, (false, false) by default
}

impl CdcSerial {
    /// Probes for CDC-ACM devices. It checks the current configuration of each device.
    /// Returns an empty vector if no device is found.
    pub fn probe() -> io::Result<Vec<DeviceInfo>> {
        #[cfg(target_os = "android")]
        let devs = usb::list_devices()?;

        #[cfg(not(target_os = "android"))]
        let devs = usb::list_devices().wait()?;

        Ok(devs
            .into_iter()
            .filter(|dev| Self::find_interfaces(dev).is_some())
            .collect())
    }

    /// Connects to the CDC-ACM device, returns the `CdcSerial` handler.
    /// Please get permission for the device before calling this function.
    /// - `timeout`: Set for standard `Read` and `Write` traits.
    pub fn build(dev_info: &DeviceInfo, timeout: Duration) -> io::Result<Self> {
        let (intr_comm, intr_data) = Self::find_interfaces(dev_info)
            .ok_or(Error::new(ErrorKind::InvalidInput, "Not a CDC-ACM device"))?;
        let ctrl_index = intr_comm.interface_number() as u16;

        #[cfg(target_os = "android")]
        let device = dev_info.open_device()?;

        #[cfg(not(target_os = "android"))]
        let device = dev_info.open().wait()?;

        let intr_comm = device
            .detach_and_claim_interface(intr_comm.interface_number())
            .wait()?;
        let intr_data = device
            .detach_and_claim_interface(intr_data.interface_number())
            .wait()?;

        // Note: It doesn't select a setting with the highest bandwidth.
        let (mut addr_r, mut addr_w) = (None, None);
        for alt in intr_data.descriptors() {
            let endps: Vec<_> = alt.endpoints().collect();
            let endp_r = endps.iter().find(|endp| endp.direction() == Direction::In);
            let endp_w = endps.iter().find(|endp| endp.direction() == Direction::Out);
            if let (Some(endp_r), Some(endp_w)) = (endp_r, endp_w) {
                addr_r = Some(endp_r.address());
                addr_w = Some(endp_w.address());
                break;
            }
        }

        let (Some(addr_r), Some(addr_w)) = (addr_r, addr_w) else {
            return Err(Error::from(ErrorKind::AddrNotAvailable));
        };

        let mut reader = EndpointRead::new(intr_data.endpoint::<Bulk, In>(addr_r)?, 1024);
        reader.set_read_timeout(timeout);

        let mut writer = EndpointWrite::new(intr_data.endpoint::<Bulk, Out>(addr_w)?, 1024);
        writer.set_write_timeout(timeout);

        #[cfg(target_os = "android")]
        let usb_path_name = dev_info.path_name().clone();

        #[cfg(not(target_os = "android"))]
        let usb_path_name = String::new(); // TODO

        Ok(Self {
            usb_path_name,
            ctrl_index,
            intr_comm,
            reader,
            writer,
            timeout,
            ser_conf: None,
            dtr_rts: (false, false),
        })
    }

    /// Returns (intr_comm, intr_data) if it is a CDC-ACM device.
    fn find_interfaces(dev_info: &DeviceInfo) -> Option<(InterfaceInfo, InterfaceInfo)> {
        let (comm, data) = (
            dev_info.interfaces().find(|intr| {
                #[cfg(target_os = "android")]
                let sub_class = intr.sub_class();

                #[cfg(not(target_os = "android"))]
                let sub_class = intr.subclass();

                intr.class() == USB_INTR_CLASS_COMM && sub_class == USB_INTR_SUBCLASS_ACM
            }),
            dev_info
                .interfaces()
                .find(|intr| intr.class() == USB_INTR_CLASS_CDC_DATA),
        );
        if let (Some(comm), Some(data)) = (comm, data) {
            Some((comm.clone(), data.clone()))
        } else {
            None
        }
    }

    /// Applies serial parameters.
    pub fn set_config(&mut self, conf: SerialConfig) -> io::Result<()> {
        let conf_bytes: [u8; 7] = conf.line_coding_bytes();
        self.control_set(SET_LINE_CODING, 0, &conf_bytes)?;
        self.ser_conf.replace(conf);
        Ok(())
    }

    /// Sets DTR and RTS states.
    fn set_dtr_rts(&mut self, dtr: bool, rts: bool) -> io::Result<()> {
        let val_dtr = if dtr { 0x1 } else { 0x0 };
        let val_rts = if rts { 0x2 } else { 0x0 };
        let val = (val_dtr | val_rts) as u16;
        self.control_set(SET_CONTROL_LINE_STATE, val, &[])?;
        self.dtr_rts = (dtr, rts);
        Ok(())
    }

    /// Sets the break state.
    fn set_break_state(&self, val: bool) -> io::Result<()> {
        let val = if val { 0xffff } else { 0 } as u16;
        self.control_set(SEND_BREAK, val, &[])
    }

    fn control_set(&self, request: u8, value: u16, buf: &[u8]) -> io::Result<()> {
        use nusb::transfer::TransferError;
        self.intr_comm
            .control_out(
                ControlOut {
                    control_type: ControlType::Class,
                    recipient: Recipient::Interface,
                    request,
                    value,
                    index: self.ctrl_index,
                    data: buf,
                },
                self.timeout * 2,
            )
            .wait()
            .map_err(|e| match e {
                TransferError::Disconnected => Error::from(ErrorKind::NotConnected),
                _ => Error::other(e),
            })
    }
}

impl Read for CdcSerial {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reader.read(buf)
    }
}

impl BufRead for CdcSerial {
    #[inline]
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.reader.fill_buf()
    }
    #[inline]
    fn consume(&mut self, amount: usize) {
        self.reader.consume(amount)
    }
}

impl Write for CdcSerial {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writer.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

impl SerialConfig {
    fn line_coding_bytes(&self) -> [u8; 7] {
        let mut bytes = [0u8; 7];
        bytes[..4].copy_from_slice(&self.baud_rate.to_le_bytes());
        bytes[4] = match self.stop_bits {
            StopBits::One => 0u8,
            StopBits::Two => 2u8,
        };
        bytes[5] = match self.parity {
            Parity::None => 0u8,
            Parity::Odd => 1u8,
            Parity::Even => 2u8,
        };
        bytes[6] = match self.data_bits {
            DataBits::Five => 5,
            DataBits::Six => 6,
            DataBits::Seven => 7,
            DataBits::Eight => 8,
        };
        bytes
    }
}

#[inline(always)]
fn err_map_to_serialport(err: Error) -> serialport::Error {
    let desc = err.to_string();
    let kind = match err.kind() {
        ErrorKind::NotConnected => serialport::ErrorKind::NoDevice,
        ErrorKind::InvalidInput => serialport::ErrorKind::InvalidInput,
        _ => serialport::ErrorKind::Io(err.kind()),
    };
    serialport::Error::new(kind, desc)
}

fn err_unsupported_op() -> serialport::Error {
    err_map_to_serialport(Error::new(
        ErrorKind::Unsupported,
        "unsupported function in trait `Serialport`",
    ))
}

impl CdcSerial {
    #[inline]
    fn get_conf_for_serialport(&self) -> Result<&SerialConfig, serialport::Error> {
        self.ser_conf.as_ref().ok_or(serialport::Error::new(
            serialport::ErrorKind::Io(std::io::ErrorKind::NotFound),
            "serial configuration haven't been set",
        ))
    }
}

impl SerialPort for CdcSerial {
    fn name(&self) -> Option<String> {
        Some(self.usb_path_name.clone())
    }

    fn baud_rate(&self) -> serialport::Result<u32> {
        Ok(self.get_conf_for_serialport()?.baud_rate)
    }
    fn data_bits(&self) -> serialport::Result<serialport::DataBits> {
        Ok(self.get_conf_for_serialport()?.data_bits)
    }
    fn parity(&self) -> serialport::Result<serialport::Parity> {
        Ok(self.get_conf_for_serialport()?.parity)
    }
    fn stop_bits(&self) -> serialport::Result<serialport::StopBits> {
        Ok(self.get_conf_for_serialport()?.stop_bits)
    }

    fn flow_control(&self) -> serialport::Result<serialport::FlowControl> {
        Ok(serialport::FlowControl::None)
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }

    fn set_baud_rate(&mut self, baud_rate: u32) -> serialport::Result<()> {
        let mut conf = self.ser_conf.unwrap_or_default();
        conf.baud_rate = baud_rate;
        self.set_config(conf).map_err(err_map_to_serialport)
    }

    fn set_data_bits(&mut self, data_bits: serialport::DataBits) -> serialport::Result<()> {
        let mut conf = self.ser_conf.unwrap_or_default();
        conf.data_bits = data_bits;
        self.set_config(conf).map_err(err_map_to_serialport)
    }

    fn set_parity(&mut self, parity: serialport::Parity) -> serialport::Result<()> {
        let mut conf = self.ser_conf.unwrap_or_default();
        conf.parity = parity;
        self.set_config(conf).map_err(err_map_to_serialport)
    }

    fn set_stop_bits(&mut self, stop_bits: serialport::StopBits) -> serialport::Result<()> {
        let mut conf = self.ser_conf.unwrap_or_default();
        conf.stop_bits = stop_bits;
        self.set_config(conf).map_err(err_map_to_serialport)
    }

    fn set_flow_control(
        &mut self,
        _flow_control: serialport::FlowControl,
    ) -> serialport::Result<()> {
        Err(err_unsupported_op())
    }

    /// Sets timeout for standard `Read` and `Write` implementations to do USB bulk transfers.
    fn set_timeout(&mut self, timeout: Duration) -> serialport::Result<()> {
        self.timeout = timeout;
        self.reader.set_read_timeout(timeout);
        self.writer.set_write_timeout(timeout);
        Ok(())
    }

    #[inline(always)]
    fn write_request_to_send(&mut self, value: bool) -> serialport::Result<()> {
        let (dtr, _) = self.dtr_rts;
        let rts = value;
        self.set_dtr_rts(dtr, rts).map_err(err_map_to_serialport)
    }

    #[inline(always)]
    fn write_data_terminal_ready(&mut self, value: bool) -> serialport::Result<()> {
        let (_, rts) = self.dtr_rts;
        let dtr = value;
        self.set_dtr_rts(dtr, rts).map_err(err_map_to_serialport)
    }

    /// Unsupported.
    fn read_clear_to_send(&mut self) -> serialport::Result<bool> {
        Err(err_unsupported_op())
    }
    /// Unsupported.
    fn read_data_set_ready(&mut self) -> serialport::Result<bool> {
        Err(err_unsupported_op())
    }
    /// Unsupported.
    fn read_ring_indicator(&mut self) -> serialport::Result<bool> {
        Err(err_unsupported_op())
    }
    /// Unsupported.
    fn read_carrier_detect(&mut self) -> serialport::Result<bool> {
        Err(err_unsupported_op())
    }

    /// Returns 0 because no buffer is maintained here, and all operations are synchronous.
    #[inline(always)]
    fn bytes_to_read(&self) -> serialport::Result<u32> {
        Ok(0)
    }
    /// Returns 0 because no buffer is maintained here, and all operations are synchronous.
    #[inline(always)]
    fn bytes_to_write(&self) -> serialport::Result<u32> {
        Ok(0)
    }
    /// Does nothing.
    fn clear(&self, _buffer_to_clear: serialport::ClearBuffer) -> serialport::Result<()> {
        Ok(())
    }

    #[inline(always)]
    fn set_break(&self) -> serialport::Result<()> {
        self.set_break_state(true).map_err(err_map_to_serialport)
    }
    #[inline(always)]
    fn clear_break(&self) -> serialport::Result<()> {
        self.set_break_state(false).map_err(err_map_to_serialport)
    }

    /// Unsupported.
    fn try_clone(&self) -> serialport::Result<Box<dyn serialport::SerialPort>> {
        Err(err_unsupported_op())
    }
}

impl UsbSerial for CdcSerial {
    fn configure(&mut self, conf: &SerialConfig) -> std::io::Result<()> {
        self.set_config(*conf)
    }

    fn sealer(_: crate::private::Internal) {}
}
