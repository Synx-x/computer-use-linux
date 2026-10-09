//! A filter for the client-to-server half of an RFB (VNC) stream.
//!
//! The daemon proxies every viewer to wayvnc. While the agent drives, a
//! viewer is view-only: the filter drops its key, pointer, clipboard and
//! resize messages and forwards the rest. Take over flips one flag, so input
//! starts on the live connection without a reconnect.
//!
//! The filter tracks message boundaries, so it must understand every message
//! type it forwards. An unknown type is an error, and the proxy then closes
//! the connection. Failing closed beats forwarding input it cannot see.

use anyhow::{bail, Result};

const MSG_SET_PIXEL_FORMAT: u8 = 0;
const MSG_SET_ENCODINGS: u8 = 2;
const MSG_UPDATE_REQUEST: u8 = 3;
const MSG_KEY: u8 = 4;
const MSG_POINTER: u8 = 5;
const MSG_CUT_TEXT: u8 = 6;
const MSG_CONTINUOUS_UPDATES: u8 = 150;
const MSG_FENCE: u8 = 248;
const MSG_XVP: u8 = 250;
const MSG_SET_DESKTOP_SIZE: u8 = 251;
const MSG_QEMU: u8 = 255;

const SECURITY_NONE: u8 = 1;
const SECURITY_VNC_AUTH: u8 = 2;
/// Cap one clipboard message so a broken client cannot make the proxy buffer
/// without limit.
const MAX_CUT_TEXT: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// "RFB 003.008\n" from the client.
    Version,
    /// One byte: the security type the client picked (3.7 and later).
    SecurityChoice,
    /// 16 bytes: the VNC-auth challenge response.
    VncAuthResponse,
    /// One byte: the shared-desktop flag.
    ClientInit,
    Messages,
}

#[derive(Debug)]
pub struct ClientFilter {
    phase: Phase,
    pending: Vec<u8>,
}

impl Default for ClientFilter {
    fn default() -> Self {
        Self {
            phase: Phase::Version,
            pending: Vec::new(),
        }
    }
}

impl ClientFilter {
    /// Feed bytes from the viewer. Returns the bytes to forward to wayvnc.
    /// With `allow_input` false, input messages are dropped whole.
    pub fn feed(&mut self, bytes: &[u8], allow_input: bool) -> Result<Vec<u8>> {
        let mut pending = std::mem::take(&mut self.pending);
        pending.extend_from_slice(bytes);
        let mut out = Vec::with_capacity(pending.len());
        let mut used = 0;
        loop {
            let rest = &pending[used..];
            let Some((length, is_input)) = self.next_length(rest)? else {
                break;
            };
            if !is_input || allow_input {
                out.extend_from_slice(&rest[..length]);
            }
            used += length;
        }
        pending.drain(..used);
        self.pending = pending;
        Ok(out)
    }

    /// The length of the next complete unit in `rest`, and whether it is
    /// input. `None` means more bytes are needed.
    fn next_length(&mut self, rest: &[u8]) -> Result<Option<(usize, bool)>> {
        match self.phase {
            Phase::Version => {
                if rest.len() < 12 {
                    return Ok(None);
                }
                if !rest.starts_with(b"RFB ") {
                    bail!("viewer did not send an RFB version");
                }
                // 3.3 has no client security choice. The server decides.
                self.phase = if &rest[4..11] == b"003.003" {
                    Phase::ClientInit
                } else {
                    Phase::SecurityChoice
                };
                Ok(Some((12, false)))
            }
            Phase::SecurityChoice => {
                let Some(&choice) = rest.first() else {
                    return Ok(None);
                };
                self.phase = match choice {
                    SECURITY_NONE => Phase::ClientInit,
                    SECURITY_VNC_AUTH => Phase::VncAuthResponse,
                    other => bail!("unsupported RFB security type {other}"),
                };
                Ok(Some((1, false)))
            }
            Phase::VncAuthResponse => {
                if rest.len() < 16 {
                    return Ok(None);
                }
                self.phase = Phase::ClientInit;
                Ok(Some((16, false)))
            }
            Phase::ClientInit => {
                if rest.is_empty() {
                    return Ok(None);
                }
                self.phase = Phase::Messages;
                Ok(Some((1, false)))
            }
            Phase::Messages => message_length(rest),
        }
    }
}

fn be_u16(bytes: &[u8], at: usize) -> usize {
    u16::from_be_bytes([bytes[at], bytes[at + 1]]) as usize
}

fn be_i32(bytes: &[u8], at: usize) -> i32 {
    i32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// Length of one client message, if complete, and whether it is input.
fn message_length(rest: &[u8]) -> Result<Option<(usize, bool)>> {
    let Some(&kind) = rest.first() else {
        return Ok(None);
    };
    // Each arm names the header size needed to know the full length.
    let fixed = |length: usize, input: bool| Ok((rest.len() >= length).then_some((length, input)));
    match kind {
        MSG_SET_PIXEL_FORMAT => fixed(20, false),
        MSG_SET_ENCODINGS => {
            if rest.len() < 4 {
                return Ok(None);
            }
            fixed(4 + 4 * be_u16(rest, 2), false)
        }
        MSG_UPDATE_REQUEST => fixed(10, false),
        MSG_KEY => fixed(8, true),
        MSG_POINTER => fixed(6, true),
        MSG_CUT_TEXT => {
            if rest.len() < 8 {
                return Ok(None);
            }
            // The extended-clipboard form sends a negative length.
            let length = be_i32(rest, 4).unsigned_abs() as usize;
            if length > MAX_CUT_TEXT {
                bail!("RFB clipboard message of {length} bytes is too large");
            }
            fixed(8 + length, true)
        }
        MSG_CONTINUOUS_UPDATES => fixed(10, false),
        MSG_FENCE => {
            if rest.len() < 9 {
                return Ok(None);
            }
            fixed(9 + rest[8] as usize, false)
        }
        MSG_XVP => fixed(4, true),
        MSG_SET_DESKTOP_SIZE => {
            if rest.len() < 8 {
                return Ok(None);
            }
            fixed(8 + 16 * rest[6] as usize, true)
        }
        MSG_QEMU => {
            if rest.len() < 2 {
                return Ok(None);
            }
            match rest[1] {
                // Extended key event.
                0 => fixed(12, true),
                subtype => bail!("unsupported QEMU RFB message subtype {subtype}"),
            }
        }
        other => bail!("unsupported RFB client message type {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handshake() -> Vec<u8> {
        let mut bytes = b"RFB 003.008\n".to_vec();
        bytes.push(SECURITY_NONE);
        bytes.push(1); // shared
        bytes
    }

    fn key_event() -> Vec<u8> {
        vec![MSG_KEY, 1, 0, 0, 0, 0, 0, 0x61]
    }

    fn pointer_event() -> Vec<u8> {
        vec![MSG_POINTER, 1, 0, 10, 0, 20]
    }

    fn update_request() -> Vec<u8> {
        vec![MSG_UPDATE_REQUEST, 1, 0, 0, 0, 0, 5, 0, 3, 32]
    }

    #[test]
    fn view_only_drops_input_and_keeps_everything_else() {
        let mut filter = ClientFilter::default();
        let mut stream = handshake();
        stream.extend(update_request());
        stream.extend(key_event());
        stream.extend(pointer_event());
        stream.extend(update_request());
        let out = filter.feed(&stream, false).unwrap();
        let mut expected = handshake();
        expected.extend(update_request());
        expected.extend(update_request());
        assert_eq!(out, expected);
    }

    #[test]
    fn control_forwards_input() {
        let mut filter = ClientFilter::default();
        let mut stream = handshake();
        stream.extend(key_event());
        stream.extend(pointer_event());
        assert_eq!(filter.feed(&stream, true).unwrap(), stream);
    }

    #[test]
    fn split_messages_wait_for_their_last_byte() {
        let mut filter = ClientFilter::default();
        assert_eq!(filter.feed(&handshake(), true).unwrap(), handshake());
        let key = key_event();
        assert!(filter.feed(&key[..3], true).unwrap().is_empty());
        assert_eq!(filter.feed(&key[3..], true).unwrap(), key);
    }

    #[test]
    fn input_toggles_between_messages_on_one_connection() {
        let mut filter = ClientFilter::default();
        filter.feed(&handshake(), false).unwrap();
        assert!(filter.feed(&key_event(), false).unwrap().is_empty());
        assert_eq!(filter.feed(&key_event(), true).unwrap(), key_event());
    }

    #[test]
    fn variable_length_messages_are_framed() {
        let mut filter = ClientFilter::default();
        filter.feed(&handshake(), false).unwrap();
        let mut encodings = vec![MSG_SET_ENCODINGS, 0, 0, 2];
        encodings.extend([0, 0, 0, 7, 0xff, 0xff, 0xff, 0x21]);
        let mut cut = vec![MSG_CUT_TEXT, 0, 0, 0, 0, 0, 0, 3];
        cut.extend(b"abc");
        let mut fence = vec![MSG_FENCE, 0, 0, 0, 0, 0, 0, 1, 2, 9, 9];
        let mut stream = encodings.clone();
        stream.extend(&cut);
        stream.append(&mut fence.clone());
        let mut expected = encodings;
        expected.append(&mut fence);
        assert_eq!(filter.feed(&stream, false).unwrap(), expected);
    }

    #[test]
    fn unknown_messages_fail_closed() {
        let mut filter = ClientFilter::default();
        filter.feed(&handshake(), true).unwrap();
        assert!(filter.feed(&[42, 0, 0], true).is_err());
    }

    #[test]
    fn vnc_auth_response_is_framed() {
        let mut filter = ClientFilter::default();
        let mut stream = b"RFB 003.008\n".to_vec();
        stream.push(SECURITY_VNC_AUTH);
        stream.extend([7u8; 16]);
        stream.push(1);
        stream.extend(key_event());
        let out = filter.feed(&stream, false).unwrap();
        assert_eq!(out.len(), stream.len() - key_event().len());
    }
}
