//! ESP32 serial output helpers.

use esp_hal::Blocking;
use esp_hal::usb_serial_jtag::{UsbSerialJtag, UsbSerialJtagRx};
use heapless::{String, Vec};
use muninn_gate_core::config::MAX_TELEMETRY_PRODUCERS;
use muninn_gate_core::output::{render_gateway_serial_json, render_serial_json};
use muninn_gate_core::{Error, TelemetrySnapshot};

use crate::provisioning::USB_CONFIG_JSON_BYTES;

/// Fixed buffer size for one rendered serial JSON event.
pub const SERIAL_JSON_BUFFER_BYTES: usize = 2048;
/// Scratch bytes read from USB Serial/JTAG in one non-blocking pass.
pub const USB_READ_CHUNK_BYTES: usize = 64;

/// Write one line to the ESP serial console.
pub fn write_line(message: &str)
{
    esp_println::println!("{}", message);
}

/// Write a multi-line block to the ESP serial console.
pub fn write_block(message: &str)
{
    for line in message.lines() {
        write_line(line);
    }
}

/// Write one telemetry snapshot as newline-delimited JSON events.
pub fn write_snapshot_json(
    snapshot: &TelemetrySnapshot<MAX_TELEMETRY_PRODUCERS>,
) -> Result<(), Error>
{
    let gateway = render_gateway_serial_json::<SERIAL_JSON_BUFFER_BYTES>(&snapshot.gateway)?;
    write_line(gateway.as_str());

    for record in snapshot.records.iter() {
        let event = render_serial_json::<SERIAL_JSON_BUFFER_BYTES>(record)?;
        write_line(event.as_str());
    }

    Ok(())
}

/// Non-blocking receiver for USB-uploaded JSON provisioning documents.
pub struct UsbJsonReceiver
{
    rx:            UsbSerialJtagRx<'static, Blocking>,
    buffer:        Vec<u8, USB_CONFIG_JSON_BYTES>,
    activity:      bool,
    depth:         u16,
    started:       bool,
    in_string:     bool,
    escaped:       bool,
    line_comment:  bool,
    pending_slash: bool,
}

impl UsbJsonReceiver
{
    /// Create a receiver from the ESP USB Serial/JTAG peripheral.
    pub fn new(serial: UsbSerialJtag<'static, Blocking>) -> Self
    {
        let (rx, _tx) = serial.split();
        Self {
            rx,
            buffer: Vec::new(),
            activity: false,
            depth: 0,
            started: false,
            in_string: false,
            escaped: false,
            line_comment: false,
            pending_slash: false,
        }
    }

    /// Poll for one complete JSON or JSONC document.
    pub fn poll_document(&mut self) -> Result<Option<String<USB_CONFIG_JSON_BYTES>>, Error>
    {
        let mut saw_byte = false;
        while let Ok(byte) = self.rx.read_byte() {
            saw_byte = true;
            if let Some(document) = self.accept_byte(byte)? {
                self.activity = false;
                return Ok(Some(document));
            }
        }

        if saw_byte {
            self.activity = true;
        }

        Ok(None)
    }

    /// Return true once when bytes have arrived without a complete document.
    pub fn take_activity(&mut self) -> bool
    {
        let activity = self.activity;
        self.activity = false;
        activity
    }

    /// Return the number of bytes buffered for the current document.
    pub fn buffered_bytes(&self) -> usize
    {
        self.buffer.len()
    }

    /// Drop an incomplete document and return to idle receive state.
    pub fn discard_partial(&mut self)
    {
        self.buffer.clear();
        self.activity = false;
        self.reset_state();
    }

    /// Take the buffered bytes as a document and return to idle receive state.
    pub fn take_buffered_document(&mut self) -> Result<String<USB_CONFIG_JSON_BYTES>, Error>
    {
        self.complete_document()
    }

    fn accept_byte(&mut self, byte: u8) -> Result<Option<String<USB_CONFIG_JSON_BYTES>>, Error>
    {
        if !self.started {
            if byte.is_ascii_whitespace() {
                return Ok(None);
            }
            self.started = true;
        }

        self.buffer.push(byte).map_err(|_| Error::Capacity)?;

        if self.line_comment {
            if byte == b'\n' {
                self.line_comment = false;
            }
            return Ok(None);
        }

        if self.in_string {
            if self.escaped {
                self.escaped = false;
            } else if byte == b'\\' {
                self.escaped = true;
            } else if byte == b'"' {
                self.in_string = false;
            }
            return Ok(None);
        }

        if self.pending_slash {
            self.pending_slash = false;
            if byte == b'/' {
                self.line_comment = true;
                return Ok(None);
            }
        }

        match byte {
            b'"' => self.in_string = true,
            b'/' => self.pending_slash = true,
            b'{' | b'[' => self.depth = self.depth.saturating_add(1),
            b'}' | b']' => {
                self.depth = self.depth.checked_sub(1).ok_or(Error::InvalidConfig)?;
                if self.depth == 0 {
                    return self.complete_document().map(Some);
                }
            },
            _ => {},
        }

        Ok(None)
    }

    fn complete_document(&mut self) -> Result<String<USB_CONFIG_JSON_BYTES>, Error>
    {
        let bytes = core::mem::take(&mut self.buffer);
        self.reset_state();
        let text = core::str::from_utf8(bytes.as_slice()).map_err(|_| Error::InvalidConfig)?;
        let mut document = String::new();
        document
            .push_str(text)
            .map_err(|_| Error::RenderBufferFull)?;
        Ok(document)
    }

    fn reset_state(&mut self)
    {
        self.depth = 0;
        self.started = false;
        self.in_string = false;
        self.escaped = false;
        self.line_comment = false;
        self.pending_slash = false;
    }
}
