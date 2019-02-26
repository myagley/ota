/*!
 * MQTT protocol types.
 */

use bytes::{Buf, BufMut, IntoBuf};

mod packet;

pub use self::packet::{
    Packet, PacketCodec, PacketIdentifierDupQoS, Publication, QoS, SubAckQos, SubscribeTo,
};

/// The client ID
///
/// Refs:
/// - 3.1.3.1 Client Identifier
/// - 3.1.2.4 Clean Session
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientId {
    ServerGenerated,
    IdWithCleanSession(String),
    IdWithExistingSession(String),
}

/// The return code for a connection attempt
///
/// Ref: 3.2.2.3 Connect Return code
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectReturnCode {
    Accepted,
    Refused(ConnectionRefusedReason),
}

/// The reason the connection was refused by the server
///
/// Ref: 3.2.2.3 Connect Return code
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionRefusedReason {
    UnacceptableProtocolVersion,
    IdentifierRejected,
    ServerUnavailable,
    BadUserNameOrPassword,
    NotAuthorized,
    Other(u8),
}

impl From<u8> for ConnectReturnCode {
    fn from(code: u8) -> Self {
        match code {
            0x00 => ConnectReturnCode::Accepted,
            0x01 => {
                ConnectReturnCode::Refused(ConnectionRefusedReason::UnacceptableProtocolVersion)
            }
            0x02 => ConnectReturnCode::Refused(ConnectionRefusedReason::IdentifierRejected),
            0x03 => ConnectReturnCode::Refused(ConnectionRefusedReason::ServerUnavailable),
            0x04 => ConnectReturnCode::Refused(ConnectionRefusedReason::BadUserNameOrPassword),
            0x05 => ConnectReturnCode::Refused(ConnectionRefusedReason::NotAuthorized),
            code => ConnectReturnCode::Refused(ConnectionRefusedReason::Other(code)),
        }
    }
}

impl From<ConnectReturnCode> for u8 {
    fn from(code: ConnectReturnCode) -> Self {
        match code {
            ConnectReturnCode::Accepted => 0x00,
            ConnectReturnCode::Refused(ConnectionRefusedReason::UnacceptableProtocolVersion) => {
                0x01
            }
            ConnectReturnCode::Refused(ConnectionRefusedReason::IdentifierRejected) => 0x02,
            ConnectReturnCode::Refused(ConnectionRefusedReason::ServerUnavailable) => 0x03,
            ConnectReturnCode::Refused(ConnectionRefusedReason::BadUserNameOrPassword) => 0x04,
            ConnectReturnCode::Refused(ConnectionRefusedReason::NotAuthorized) => 0x05,
            ConnectReturnCode::Refused(ConnectionRefusedReason::Other(code)) => code,
        }
    }
}

/// A tokio codec that encodes and decodes MQTT-format strings.
///
/// Strings are prefixed with a two-byte big-endian length and are encoded as utf-8.
///
/// Ref: 1.5.3 UTF-8 encoded strings
#[derive(Debug, Default)]
pub struct Utf8StringCodec {
    decoder_state: Utf8StringDecoderState,
}

#[derive(Debug)]
pub enum Utf8StringDecoderState {
    Empty,
    HaveLength(usize),
}

impl Default for Utf8StringDecoderState {
    fn default() -> Self {
        Utf8StringDecoderState::Empty
    }
}

impl tokio::codec::Decoder for Utf8StringCodec {
    type Item = String;
    type Error = DecodeError;

    fn decode(&mut self, src: &mut bytes::BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        loop {
            match &mut self.decoder_state {
                Utf8StringDecoderState::Empty => {
                    let len = match src.try_get_u16_be() {
                        Ok(len) => len as usize,
                        Err(_) => return Ok(None),
                    };
                    self.decoder_state = Utf8StringDecoderState::HaveLength(len);
                }

                Utf8StringDecoderState::HaveLength(len) => {
                    if src.len() < *len {
                        return Ok(None);
                    }

                    let s = match std::str::from_utf8(&src.split_to(*len)) {
                        Ok(s) => s.to_string(),
                        Err(err) => return Err(DecodeError::StringNotUtf8(err)),
                    };
                    self.decoder_state = Utf8StringDecoderState::Empty;
                    return Ok(Some(s));
                }
            }
        }
    }
}

impl tokio::codec::Encoder for Utf8StringCodec {
    type Item = String;
    type Error = EncodeError;

    fn encode(&mut self, item: Self::Item, dst: &mut bytes::BytesMut) -> Result<(), Self::Error> {
        dst.reserve(std::mem::size_of::<u16>() + item.len());

        #[allow(clippy::cast_possible_truncation)]
        match item.len() {
            len if len <= u16::max_value() as usize => dst.put_u16_be(len as u16),
            len => return Err(EncodeError::StringTooLarge(len)),
        }

        dst.put_slice(item.as_bytes());

        Ok(())
    }
}

/// A tokio codec that encodes and decodes MQTT-format "remaining length" numbers.
///
/// These numbers are encoded with a variable-length scheme that uses the MSB of each byte as a continuation bit.
///
/// Ref: 2.2.3 Remaining Length
#[derive(Debug, Default)]
pub struct RemainingLengthCodec {
    decoder_state: RemainingLengthDecoderState,
}

#[derive(Debug)]
pub struct RemainingLengthDecoderState {
    result: usize,
    num_bytes_read: usize,
}

impl Default for RemainingLengthDecoderState {
    fn default() -> Self {
        RemainingLengthDecoderState {
            result: 0,
            num_bytes_read: 0,
        }
    }
}

impl tokio::codec::Decoder for RemainingLengthCodec {
    type Item = usize;
    type Error = DecodeError;

    fn decode(&mut self, src: &mut bytes::BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        loop {
            let encoded_byte = match src.try_get_u8() {
                Ok(encoded_byte) => encoded_byte,
                Err(_) => return Ok(None),
            };

            self.decoder_state.result |=
                ((encoded_byte & 0x7F) as usize) << (self.decoder_state.num_bytes_read * 7);
            self.decoder_state.num_bytes_read += 1;

            if encoded_byte & 0x80 == 0 {
                let result = self.decoder_state.result;
                self.decoder_state = Default::default();
                return Ok(Some(result));
            }

            if self.decoder_state.num_bytes_read == 4 {
                return Err(DecodeError::RemainingLengthTooHigh);
            }
        }
    }
}

impl tokio::codec::Encoder for RemainingLengthCodec {
    type Item = usize;
    type Error = EncodeError;

    fn encode(
        &mut self,
        mut item: Self::Item,
        dst: &mut bytes::BytesMut,
    ) -> Result<(), Self::Error> {
        dst.reserve(4 * std::mem::size_of::<u8>());

        let original = item;

        loop {
            #[allow(clippy::cast_possible_truncation)]
            let mut encoded_byte = (item & 0x7F) as u8;

            item >>= 7;

            if item > 0 {
                encoded_byte |= 0x80;
            }

            dst.put_u8(encoded_byte);

            if item == 0 {
                break;
            }

            if dst.len() == 4 {
                return Err(EncodeError::RemainingLengthTooHigh(original));
            }
        }

        Ok(())
    }
}

/// A packet identifier. Two-byte unsigned integer that cannot be zero.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PacketIdentifier(u16);

impl PacketIdentifier {
    /// Returns the largest value that is a valid packet identifier.
    pub const fn max_value() -> Self {
        PacketIdentifier(u16::max_value())
    }

    /// Convert the given raw packet identifier into this type.
    #[allow(clippy::new_ret_no_self)] // Clippy bug
    pub fn new(raw: u16) -> Option<Self> {
        match raw {
            0 => None,
            raw => Some(PacketIdentifier(raw)),
        }
    }

    /// Get the raw packet identifier.
    pub fn get(self) -> u16 {
        self.0
    }
}

impl std::fmt::Display for PacketIdentifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::ops::Add<u16> for PacketIdentifier {
    type Output = Self;

    fn add(self, other: u16) -> Self::Output {
        PacketIdentifier(match self.0.wrapping_add(other) {
            0 => 1,
            value => value,
        })
    }
}

impl std::ops::AddAssign<u16> for PacketIdentifier {
    fn add_assign(&mut self, other: u16) {
        *self = *self + other;
    }
}

#[derive(Debug)]
pub enum DecodeError {
    ConnectReservedSet,
    IncompletePacket,
    Io(std::io::Error),
    PublishDupAtMostOnce,
    NoTopics,
    RemainingLengthTooHigh,
    StringNotUtf8(std::str::Utf8Error),
    UnrecognizedConnAckFlags(u8),
    UnrecognizedPacket {
        packet_type: u8,
        flags: u8,
        remaining_length: usize,
    },
    UnrecognizedProtocolLevel(u8),
    UnrecognizedProtocolName(String),
    UnrecognizedQoS(u8),
    ZeroPacketIdentifier,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::ConnectReservedSet => {
                write!(f, "the reserved byte of the CONNECT flags is set")
            }
            DecodeError::IncompletePacket => write!(f, "packet is truncated"),
            DecodeError::Io(err) => write!(f, "I/O error: {}", err),
            DecodeError::NoTopics => write!(f, "expected at least one topic but there were none"),
            DecodeError::PublishDupAtMostOnce => {
                write!(f, "PUBLISH packet has DUP flag set and QoS 0")
            }
            DecodeError::RemainingLengthTooHigh => {
                write!(f, "remaining length is too high to be decoded")
            }
            DecodeError::StringNotUtf8(err) => err.fmt(f),
            DecodeError::UnrecognizedConnAckFlags(flags) => {
                write!(f, "could not parse CONNACK flags 0x{:02X}", flags)
            }
            DecodeError::UnrecognizedPacket {
                packet_type,
                flags,
                remaining_length,
            } => write!(
					f,
					"could not identify packet with type 0x{:1X}, flags 0x{:1X} and remaining length {}",
					packet_type,
					flags,
					remaining_length,
				),
            DecodeError::UnrecognizedProtocolLevel(level) => {
                write!(f, "unexpected protocol level {:?}", level)
            }
            DecodeError::UnrecognizedProtocolName(name) => {
                write!(f, "unexpected protocol name {:?}", name)
            }
            DecodeError::UnrecognizedQoS(qos) => write!(f, "could not parse QoS 0x{:02X}", qos),
            DecodeError::ZeroPacketIdentifier => write!(f, "packet identifier is 0"),
        }
    }
}

impl std::error::Error for DecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        #[allow(clippy::match_same_arms)]
        match self {
            DecodeError::ConnectReservedSet => None,
            DecodeError::IncompletePacket => None,
            DecodeError::Io(err) => Some(err),
            DecodeError::NoTopics => None,
            DecodeError::PublishDupAtMostOnce => None,
            DecodeError::RemainingLengthTooHigh => None,
            DecodeError::StringNotUtf8(err) => Some(err),
            DecodeError::UnrecognizedConnAckFlags(_) => None,
            DecodeError::UnrecognizedPacket { .. } => None,
            DecodeError::UnrecognizedProtocolLevel(_) => None,
            DecodeError::UnrecognizedProtocolName(_) => None,
            DecodeError::UnrecognizedQoS(_) => None,
            DecodeError::ZeroPacketIdentifier => None,
        }
    }
}

impl From<std::io::Error> for DecodeError {
    fn from(err: std::io::Error) -> Self {
        DecodeError::Io(err)
    }
}

#[derive(Debug)]
pub enum EncodeError {
    Io(std::io::Error),
    KeepAliveTooHigh(std::time::Duration),
    RemainingLengthTooHigh(usize),
    StringTooLarge(usize),
    WillTooLarge(usize),
}

impl EncodeError {
    pub fn is_user_error(&self) -> bool {
        #[allow(clippy::match_same_arms)]
        match self {
            EncodeError::Io(_) => false,
            EncodeError::KeepAliveTooHigh(_) => true,
            EncodeError::RemainingLengthTooHigh(_) => true,
            EncodeError::StringTooLarge(_) => true,
            EncodeError::WillTooLarge(_) => true,
        }
    }
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncodeError::Io(err) => write!(f, "I/O error: {}", err),
            EncodeError::KeepAliveTooHigh(keep_alive) => {
                write!(f, "keep-alive {:?} is too high", keep_alive)
            }
            EncodeError::RemainingLengthTooHigh(len) => {
                write!(f, "remaining length {} is too high to be encoded", len)
            }
            EncodeError::StringTooLarge(len) => {
                write!(f, "string of length {} is too large to be encoded", len)
            }
            EncodeError::WillTooLarge(len) => write!(
                f,
                "will payload of length {} is too large to be encoded",
                len
            ),
        }
    }
}

impl std::error::Error for EncodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        #[allow(clippy::match_same_arms)]
        match self {
            EncodeError::Io(err) => Some(err),
            EncodeError::KeepAliveTooHigh(_) => None,
            EncodeError::RemainingLengthTooHigh(_) => None,
            EncodeError::StringTooLarge(_) => None,
            EncodeError::WillTooLarge(_) => None,
        }
    }
}

impl From<std::io::Error> for EncodeError {
    fn from(err: std::io::Error) -> Self {
        EncodeError::Io(err)
    }
}

trait BufMutExt {
    fn try_get_u8(&mut self) -> Result<u8, DecodeError>;
    fn try_get_u16_be(&mut self) -> Result<u16, DecodeError>;

    fn append_u8(&mut self, n: u8);
    fn append_u16_be(&mut self, n: u16);
    fn append_packet_identifier(&mut self, packet_identifier: PacketIdentifier);
}

impl BufMutExt for bytes::BytesMut {
    fn try_get_u8(&mut self) -> Result<u8, DecodeError> {
        if self.len() < std::mem::size_of::<u8>() {
            return Err(DecodeError::IncompletePacket);
        }

        let result = self[0];
        self.advance(std::mem::size_of::<u8>());
        Ok(result)
    }

    fn try_get_u16_be(&mut self) -> Result<u16, DecodeError> {
        if self.len() < std::mem::size_of::<u16>() {
            return Err(DecodeError::IncompletePacket);
        }

        Ok(self
            .split_to(std::mem::size_of::<u16>())
            .into_buf()
            .get_u16_be())
    }

    fn append_u8(&mut self, n: u8) {
        self.reserve(std::mem::size_of::<u8>());
        self.put_u8(n);
    }

    fn append_u16_be(&mut self, n: u16) {
        self.reserve(std::mem::size_of::<u16>());
        self.put_u16_be(n);
    }

    fn append_packet_identifier(&mut self, packet_identifier: PacketIdentifier) {
        self.reserve(std::mem::size_of::<u16>());
        self.put_u16_be(packet_identifier.0);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn remaining_length_encode() {
        remaining_length_encode_inner_ok(0x00, &[0x00]);
        remaining_length_encode_inner_ok(0x01, &[0x01]);

        remaining_length_encode_inner_ok(0x7F, &[0x7F]);
        remaining_length_encode_inner_ok(0x80, &[0x80, 0x01]);
        remaining_length_encode_inner_ok(0x3FFF, &[0xFF, 0x7F]);
        remaining_length_encode_inner_ok(0x4000, &[0x80, 0x80, 0x01]);
        remaining_length_encode_inner_ok(0x001F_FFFF, &[0xFF, 0xFF, 0x7F]);
        remaining_length_encode_inner_ok(0x0020_0000, &[0x80, 0x80, 0x80, 0x01]);
        remaining_length_encode_inner_ok(0x0FFF_FFFF, &[0xFF, 0xFF, 0xFF, 0x7F]);

        remaining_length_encode_inner_too_high(0x1000_0000);
        remaining_length_encode_inner_too_high(0xFFFF_FFFF);
        remaining_length_encode_inner_too_high(0xFFFF_FFFF_FFFF_FFFF);
    }

    fn remaining_length_encode_inner_ok(value: usize, expected: &[u8]) {
        use tokio::codec::Encoder;

        let mut bytes = bytes::BytesMut::new();
        super::RemainingLengthCodec::default()
            .encode(value, &mut bytes)
            .unwrap();
        assert_eq!(&*bytes, expected);
    }

    fn remaining_length_encode_inner_too_high(value: usize) {
        use tokio::codec::Encoder;

        let mut bytes = bytes::BytesMut::new();
        let err = super::RemainingLengthCodec::default()
            .encode(value, &mut bytes)
            .unwrap_err();
        if let super::EncodeError::RemainingLengthTooHigh(v) = err {
            assert_eq!(v, value);
        } else {
            panic!("{:?}", err);
        }
    }

    #[test]
    fn remaining_length_decode() {
        remaining_length_decode_inner_ok(&[0x00], 0x00);
        remaining_length_decode_inner_ok(&[0x01], 0x01);

        remaining_length_decode_inner_ok(&[0x7F], 0x7F);
        remaining_length_decode_inner_ok(&[0x80, 0x01], 0x80);
        remaining_length_decode_inner_ok(&[0xFF, 0x7F], 0x3FFF);
        remaining_length_decode_inner_ok(&[0x80, 0x80, 0x01], 0x4000);
        remaining_length_decode_inner_ok(&[0xFF, 0xFF, 0x7F], 0x001F_FFFF);
        remaining_length_decode_inner_ok(&[0x80, 0x80, 0x80, 0x01], 0x0020_0000);
        remaining_length_decode_inner_ok(&[0xFF, 0xFF, 0xFF, 0x7F], 0x0FFF_FFFF);

        // Longer-than-necessary encodings are not disallowed by the spec
        remaining_length_decode_inner_ok(&[0x81, 0x00], 0x01);
        remaining_length_decode_inner_ok(&[0x81, 0x80, 0x00], 0x01);
        remaining_length_decode_inner_ok(&[0x81, 0x80, 0x80, 0x00], 0x01);

        remaining_length_decode_inner_too_high(&[0x80, 0x80, 0x80, 0x80]);
        remaining_length_decode_inner_too_high(&[0xFF, 0xFF, 0xFF, 0xFF]);

        remaining_length_decode_inner_incomplete_packet(&[0x80]);
        remaining_length_decode_inner_incomplete_packet(&[0x80, 0x80]);
        remaining_length_decode_inner_incomplete_packet(&[0x80, 0x80, 0x80]);
    }

    fn remaining_length_decode_inner_ok(bytes: &[u8], expected: usize) {
        use tokio::codec::Decoder;

        let mut bytes = bytes::BytesMut::from(bytes);
        let actual = super::RemainingLengthCodec::default()
            .decode(&mut bytes)
            .unwrap()
            .unwrap();
        assert_eq!(actual, expected);
        assert!(bytes.is_empty());
    }

    fn remaining_length_decode_inner_too_high(bytes: &[u8]) {
        use tokio::codec::Decoder;

        let mut bytes = bytes::BytesMut::from(bytes);
        let err = super::RemainingLengthCodec::default()
            .decode(&mut bytes)
            .unwrap_err();
        if let super::DecodeError::RemainingLengthTooHigh = err {
        } else {
            panic!("{:?}", err);
        }
    }

    fn remaining_length_decode_inner_incomplete_packet(bytes: &[u8]) {
        use tokio::codec::Decoder;

        let mut bytes = bytes::BytesMut::from(bytes);
        assert_eq!(
            super::RemainingLengthCodec::default()
                .decode(&mut bytes)
                .unwrap(),
            None
        );
    }
}
