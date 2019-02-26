use super::BufMutExt;

/// An MQTT packet
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Packet {
    /// Ref: 3.2 CONNACK – Acknowledge connection request
    ConnAck {
        session_present: bool,
        return_code: super::ConnectReturnCode,
    },

    /// Ref: 3.1 CONNECT – Client requests a connection to a Server
    Connect {
        username: Option<String>,
        password: Option<String>,
        will: Option<Publication>,
        client_id: super::ClientId,
        keep_alive: std::time::Duration,
    },

    Disconnect,

    /// Ref: 3.12 PINGREQ – PING request
    PingReq,

    /// Ref: 3.13 PINGRESP – PING response
    PingResp,

    /// Ref: 3.4 PUBACK – Publish acknowledgement
    PubAck {
        packet_identifier: super::PacketIdentifier,
    },

    /// Ref: 3.7 PUBCOMP – Publish complete (QoS 2 publish received, part 3)
    PubComp {
        packet_identifier: super::PacketIdentifier,
    },

    /// 3.3 PUBLISH – Publish message
    Publish {
        packet_identifier_dup_qos: PacketIdentifierDupQoS,
        retain: bool,
        topic_name: String,
        payload: Vec<u8>,
    },

    /// Ref: 3.5 PUBREC – Publish received (QoS 2 publish received, part 1)
    PubRec {
        packet_identifier: super::PacketIdentifier,
    },

    /// Ref: 3.6 PUBREL – Publish release (QoS 2 publish received, part 2)
    PubRel {
        packet_identifier: super::PacketIdentifier,
    },

    /// Ref: 3.9 SUBACK – Subscribe acknowledgement
    SubAck {
        packet_identifier: super::PacketIdentifier,
        qos: Vec<SubAckQos>,
    },

    /// Ref: 3.8 SUBSCRIBE - Subscribe to topics
    Subscribe {
        packet_identifier: super::PacketIdentifier,
        subscribe_to: Vec<SubscribeTo>,
    },

    /// Ref: 3.11 UNSUBACK – Unsubscribe acknowledgement
    UnsubAck {
        packet_identifier: super::PacketIdentifier,
    },

    /// Ref: 3.10 UNSUBSCRIBE – Unsubscribe from topics
    Unsubscribe {
        packet_identifier: super::PacketIdentifier,
        unsubscribe_from: Vec<String>,
    },
}

impl Packet {
    /// The type of a [`Packet::ConnAck`]
    pub const CONNACK: u8 = 0x20;

    /// The type of a [`Packet::Connect`]
    pub const CONNECT: u8 = 0x10;

    /// The type of a [`Packet::Disconnect`]
    pub const DISCONNECT: u8 = 0xE0;

    /// The type of a [`Packet::PingReq`]
    pub const PINGREQ: u8 = 0xC0;

    /// The type of a [`Packet::PingResp`]
    pub const PINGRESP: u8 = 0xD0;

    /// The type of a [`Packet::PubAck`]
    pub const PUBACK: u8 = 0x40;

    /// The type of a [`Packet::PubComp`]
    pub const PUBCOMP: u8 = 0x70;

    /// The type of a [`Packet::Publish`]
    pub const PUBLISH: u8 = 0x30;

    /// The type of a [`Packet::PubRec`]
    pub const PUBREC: u8 = 0x50;

    /// The type of a [`Packet::PubRel`]
    pub const PUBREL: u8 = 0x60;

    /// The type of a [`Packet::SubAck`]
    pub const SUBACK: u8 = 0x90;

    /// The type of a [`Packet::Subscribe`]
    pub const SUBSCRIBE: u8 = 0x80;

    /// The type of a [`Packet::UnsubAck`]
    pub const UNSUBACK: u8 = 0xB0;

    /// The type of a [`Packet::Unsubscribe`]
    pub const UNSUBSCRIBE: u8 = 0xA0;
}

#[allow(clippy::doc_markdown)]
/// A combination of the packet identifier, dup flag and QoS that only allows valid combinations of these three properties.
/// Used in [`Packet::Publish`]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PacketIdentifierDupQoS {
    AtMostOnce,
    AtLeastOnce(super::PacketIdentifier, bool),
    ExactlyOnce(super::PacketIdentifier, bool),
}

/// A subscription request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubscribeTo {
    pub topic_filter: String,
    pub qos: QoS,
}

/// The level of reliability for a publication
///
/// Ref: 4.3 Quality of Service levels and protocol flows
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum QoS {
    AtMostOnce,
    AtLeastOnce,
    ExactlyOnce,
}

impl From<QoS> for u8 {
    fn from(qos: QoS) -> Self {
        match qos {
            QoS::AtMostOnce => 0x00,
            QoS::AtLeastOnce => 0x01,
            QoS::ExactlyOnce => 0x02,
        }
    }
}

#[allow(clippy::doc_markdown)]
/// QoS returned in a SUBACK packet. Either one of the [`QoS`] values, or an error code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubAckQos {
    Success(QoS),
    Failure,
}

impl From<SubAckQos> for u8 {
    fn from(qos: SubAckQos) -> Self {
        match qos {
            SubAckQos::Success(qos) => qos.into(),
            SubAckQos::Failure => 0x80,
        }
    }
}

/// A message that can be published to the server
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Publication {
    pub topic_name: String,
    pub qos: crate::proto::QoS,
    pub retain: bool,
    pub payload: Vec<u8>,
}

/// A tokio codec that encodes and decodes MQTT packets.
///
/// Ref: 2 MQTT Control Packet format
#[derive(Debug, Default)]
pub struct PacketCodec {
    decoder_state: PacketDecoderState,
}

#[derive(Debug)]
pub enum PacketDecoderState {
    Empty,
    HaveFirstByte {
        first_byte: u8,
        remaining_length: super::RemainingLengthCodec,
    },
    HaveFixedHeader {
        first_byte: u8,
        remaining_length: usize,
    },
}

impl Default for PacketDecoderState {
    fn default() -> Self {
        PacketDecoderState::Empty
    }
}

impl tokio::codec::Decoder for PacketCodec {
    type Item = Packet;
    type Error = super::DecodeError;

    fn decode(&mut self, src: &mut bytes::BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        let (first_byte, mut src) = loop {
            match &mut self.decoder_state {
                PacketDecoderState::Empty => {
                    let first_byte = match src.try_get_u8() {
                        Ok(first_byte) => first_byte,
                        Err(_) => return Ok(None),
                    };
                    self.decoder_state = PacketDecoderState::HaveFirstByte {
                        first_byte,
                        remaining_length: Default::default(),
                    };
                }

                PacketDecoderState::HaveFirstByte {
                    first_byte,
                    remaining_length,
                } => match remaining_length.decode(src)? {
                    Some(remaining_length) => {
                        self.decoder_state = PacketDecoderState::HaveFixedHeader {
                            first_byte: *first_byte,
                            remaining_length,
                        }
                    }
                    None => return Ok(None),
                },

                PacketDecoderState::HaveFixedHeader {
                    first_byte,
                    remaining_length,
                } => {
                    if src.len() < *remaining_length {
                        return Ok(None);
                    }

                    let first_byte = *first_byte;
                    let src = src.split_to(*remaining_length);
                    self.decoder_state = PacketDecoderState::Empty;
                    break (first_byte, src);
                }
            }
        };

        match (first_byte & 0xF0, first_byte & 0x0F, src.len()) {
            (Packet::CONNACK, 0, 2) => {
                let flags = src.try_get_u8()?;
                let session_present = match flags {
                    0x00 => false,
                    0x01 => true,
                    flags => return Err(super::DecodeError::UnrecognizedConnAckFlags(flags)),
                };

                let return_code: super::ConnectReturnCode = src.try_get_u8()?.into();

                Ok(Some(Packet::ConnAck {
                    session_present,
                    return_code,
                }))
            }

            (Packet::CONNECT, 0, _) => {
                let protocol_name = super::Utf8StringCodec::default()
                    .decode(&mut src)?
                    .ok_or(super::DecodeError::IncompletePacket)?;
                if protocol_name != "MQTT" {
                    return Err(super::DecodeError::UnrecognizedProtocolName(protocol_name));
                }

                let protocol_level = src.try_get_u8()?;
                if protocol_level != 0x04 {
                    return Err(super::DecodeError::UnrecognizedProtocolLevel(
                        protocol_level,
                    ));
                }

                let connect_flags = src.try_get_u8()?;
                if connect_flags & 0x01 != 0 {
                    return Err(super::DecodeError::ConnectReservedSet);
                }

                let keep_alive = std::time::Duration::from_secs(u64::from(src.try_get_u16_be()?));

                let client_id = super::Utf8StringCodec::default()
                    .decode(&mut src)?
                    .ok_or(super::DecodeError::IncompletePacket)?;
                let client_id = if client_id == "" {
                    super::ClientId::ServerGenerated
                } else if connect_flags & 0x02 == 0 {
                    super::ClientId::IdWithExistingSession(client_id)
                } else {
                    super::ClientId::IdWithCleanSession(client_id)
                };

                let will = if connect_flags & 0x04 == 0 {
                    None
                } else {
                    let topic_name = super::Utf8StringCodec::default()
                        .decode(&mut src)?
                        .ok_or(super::DecodeError::IncompletePacket)?;

                    let qos = match connect_flags & 0x18 {
                        0x00 => QoS::AtMostOnce,
                        0x08 => QoS::AtLeastOnce,
                        0x10 => QoS::ExactlyOnce,
                        qos => return Err(super::DecodeError::UnrecognizedQoS(qos >> 3)),
                    };

                    let retain = connect_flags & 0x20 != 0;

                    let payload_len = usize::from(src.try_get_u16_be()?);
                    if src.len() < payload_len {
                        return Err(super::DecodeError::IncompletePacket);
                    }
                    let payload = (&*src.split_to(payload_len)).to_owned();

                    Some(Publication {
                        topic_name,
                        qos,
                        retain,
                        payload,
                    })
                };

                let username = if connect_flags & 0x80 == 0 {
                    None
                } else {
                    Some(
                        super::Utf8StringCodec::default()
                            .decode(&mut src)?
                            .ok_or(super::DecodeError::IncompletePacket)?,
                    )
                };

                let password = if connect_flags & 0x40 == 0 {
                    None
                } else {
                    Some(
                        super::Utf8StringCodec::default()
                            .decode(&mut src)?
                            .ok_or(super::DecodeError::IncompletePacket)?,
                    )
                };

                Ok(Some(Packet::Connect {
                    username,
                    password,
                    will,
                    client_id,
                    keep_alive,
                }))
            }

            (Packet::DISCONNECT, 0, 0) => Ok(Some(Packet::Disconnect)),

            (Packet::PINGREQ, 0, 0) => Ok(Some(Packet::PingReq)),

            (Packet::PINGRESP, 0, 0) => Ok(Some(Packet::PingResp)),

            (Packet::PUBACK, 0, 2) => {
                let packet_identifier = src.try_get_u16_be()?;
                let packet_identifier = match super::PacketIdentifier::new(packet_identifier) {
                    Some(packet_identifier) => packet_identifier,
                    None => return Err(super::DecodeError::ZeroPacketIdentifier),
                };

                Ok(Some(Packet::PubAck { packet_identifier }))
            }

            (Packet::PUBCOMP, 0, 2) => {
                let packet_identifier = src.try_get_u16_be()?;
                let packet_identifier = match super::PacketIdentifier::new(packet_identifier) {
                    Some(packet_identifier) => packet_identifier,
                    None => return Err(super::DecodeError::ZeroPacketIdentifier),
                };

                Ok(Some(Packet::PubComp { packet_identifier }))
            }

            (Packet::PUBLISH, flags, _) => {
                let dup = (flags & 0x08) != 0;
                let retain = (flags & 0x01) != 0;

                let topic_name = super::Utf8StringCodec::default()
                    .decode(&mut src)?
                    .ok_or(super::DecodeError::IncompletePacket)?;

                let packet_identifier_dup_qos = match (flags & 0x06) >> 1 {
                    0x00 if dup => return Err(super::DecodeError::PublishDupAtMostOnce),

                    0x00 => PacketIdentifierDupQoS::AtMostOnce,

                    0x01 => {
                        let packet_identifier = src.try_get_u16_be()?;
                        let packet_identifier =
                            match super::PacketIdentifier::new(packet_identifier) {
                                Some(packet_identifier) => packet_identifier,
                                None => return Err(super::DecodeError::ZeroPacketIdentifier),
                            };
                        PacketIdentifierDupQoS::AtLeastOnce(packet_identifier, dup)
                    }

                    0x02 => {
                        let packet_identifier = src.try_get_u16_be()?;
                        let packet_identifier =
                            match super::PacketIdentifier::new(packet_identifier) {
                                Some(packet_identifier) => packet_identifier,
                                None => return Err(super::DecodeError::ZeroPacketIdentifier),
                            };
                        PacketIdentifierDupQoS::ExactlyOnce(packet_identifier, dup)
                    }

                    qos => return Err(super::DecodeError::UnrecognizedQoS(qos)),
                };

                let mut payload = vec![0_u8; src.len()];
                payload.copy_from_slice(&src.take());

                Ok(Some(Packet::Publish {
                    packet_identifier_dup_qos,
                    retain,
                    topic_name,
                    payload,
                }))
            }

            (Packet::PUBREC, 0, 2) => {
                let packet_identifier = src.try_get_u16_be()?;
                let packet_identifier = match super::PacketIdentifier::new(packet_identifier) {
                    Some(packet_identifier) => packet_identifier,
                    None => return Err(super::DecodeError::ZeroPacketIdentifier),
                };

                Ok(Some(Packet::PubRec { packet_identifier }))
            }

            (Packet::PUBREL, 2, 2) => {
                let packet_identifier = src.try_get_u16_be()?;
                let packet_identifier = match super::PacketIdentifier::new(packet_identifier) {
                    Some(packet_identifier) => packet_identifier,
                    None => return Err(super::DecodeError::ZeroPacketIdentifier),
                };

                Ok(Some(Packet::PubRel { packet_identifier }))
            }

            (Packet::SUBACK, 0, remaining_length) => {
                let packet_identifier = src.try_get_u16_be()?;
                let packet_identifier = match super::PacketIdentifier::new(packet_identifier) {
                    Some(packet_identifier) => packet_identifier,
                    None => return Err(super::DecodeError::ZeroPacketIdentifier),
                };

                let mut qos = vec![];
                for _ in 2..remaining_length {
                    qos.push(match src.try_get_u8()? {
                        0x00 => SubAckQos::Success(QoS::AtMostOnce),
                        0x01 => SubAckQos::Success(QoS::AtLeastOnce),
                        0x02 => SubAckQos::Success(QoS::ExactlyOnce),
                        0x80 => SubAckQos::Failure,
                        qos => return Err(super::DecodeError::UnrecognizedQoS(qos)),
                    });
                }

                Ok(Some(Packet::SubAck {
                    packet_identifier,
                    qos,
                }))
            }

            (Packet::SUBSCRIBE, 2, _) => {
                let packet_identifier = src.try_get_u16_be()?;
                let packet_identifier = match super::PacketIdentifier::new(packet_identifier) {
                    Some(packet_identifier) => packet_identifier,
                    None => return Err(super::DecodeError::ZeroPacketIdentifier),
                };

                let mut subscribe_to = vec![];

                while !src.is_empty() {
                    let topic_filter = super::Utf8StringCodec::default()
                        .decode(&mut src)?
                        .ok_or(super::DecodeError::IncompletePacket)?;
                    let qos = match src.try_get_u8()? {
                        0x00 => QoS::AtMostOnce,
                        0x01 => QoS::AtLeastOnce,
                        0x02 => QoS::ExactlyOnce,
                        qos => return Err(super::DecodeError::UnrecognizedQoS(qos)),
                    };
                    subscribe_to.push(SubscribeTo { topic_filter, qos });
                }

                if subscribe_to.is_empty() {
                    return Err(super::DecodeError::NoTopics);
                }

                Ok(Some(Packet::Subscribe {
                    packet_identifier,
                    subscribe_to,
                }))
            }

            (Packet::UNSUBACK, 0, 2) => {
                let packet_identifier = src.try_get_u16_be()?;
                let packet_identifier = match super::PacketIdentifier::new(packet_identifier) {
                    Some(packet_identifier) => packet_identifier,
                    None => return Err(super::DecodeError::ZeroPacketIdentifier),
                };

                Ok(Some(Packet::UnsubAck { packet_identifier }))
            }

            (Packet::UNSUBSCRIBE, 2, _) => {
                let packet_identifier = src.try_get_u16_be()?;
                let packet_identifier = match super::PacketIdentifier::new(packet_identifier) {
                    Some(packet_identifier) => packet_identifier,
                    None => return Err(super::DecodeError::ZeroPacketIdentifier),
                };

                let mut unsubscribe_from = vec![];

                while !src.is_empty() {
                    unsubscribe_from.push(
                        super::Utf8StringCodec::default()
                            .decode(&mut src)?
                            .ok_or(super::DecodeError::IncompletePacket)?,
                    );
                }

                if unsubscribe_from.is_empty() {
                    return Err(super::DecodeError::NoTopics);
                }

                Ok(Some(Packet::Unsubscribe {
                    packet_identifier,
                    unsubscribe_from,
                }))
            }

            (packet_type, flags, remaining_length) => Err(super::DecodeError::UnrecognizedPacket {
                packet_type,
                flags,
                remaining_length,
            }),
        }
    }
}

impl tokio::codec::Encoder for PacketCodec {
    type Item = Packet;
    type Error = super::EncodeError;

    fn encode(&mut self, item: Self::Item, dst: &mut bytes::BytesMut) -> Result<(), Self::Error> {
        dst.reserve(std::mem::size_of::<u8>() + 4 * std::mem::size_of::<u8>());

        match item {
            Packet::ConnAck {
                session_present,
                return_code,
            } => encode_packet(dst, Packet::CONNACK, |dst| {
                if session_present {
                    dst.append_u8(0x01);
                } else {
                    dst.append_u8(0x00);
                }

                dst.append_u8(return_code.into());

                Ok(())
            })?,

            Packet::Connect {
                username,
                password,
                will,
                client_id,
                keep_alive,
            } => encode_packet(dst, Packet::CONNECT, |dst| {
                super::Utf8StringCodec::default().encode("MQTT".to_string(), dst)?;

                dst.append_u8(0x04_u8);

                {
                    let mut connect_flags = 0x00_u8;
                    if username.is_some() {
                        connect_flags |= 0x80;
                    }
                    if password.is_some() {
                        connect_flags |= 0x40;
                    }
                    if let Some(will) = &will {
                        if will.retain {
                            connect_flags |= 0x20;
                        }
                        connect_flags |= match will.qos {
                            QoS::AtMostOnce => 0x00,
                            QoS::AtLeastOnce => 0x08,
                            QoS::ExactlyOnce => 0x10,
                        };
                        connect_flags |= 0x04;
                    }
                    match client_id {
                        super::ClientId::ServerGenerated
                        | super::ClientId::IdWithCleanSession(_) => connect_flags |= 0x02,
                        super::ClientId::IdWithExistingSession(_) => (),
                    }
                    dst.append_u8(connect_flags);
                }

                {
                    #[allow(clippy::cast_possible_truncation)]
                    let keep_alive = match keep_alive {
                        keep_alive if keep_alive.as_secs() <= u64::from(u16::max_value()) => {
                            keep_alive.as_secs() as u16
                        }
                        keep_alive => return Err(super::EncodeError::KeepAliveTooHigh(keep_alive)),
                    };
                    dst.append_u16_be(keep_alive);
                }

                match client_id {
                    super::ClientId::ServerGenerated => {
                        super::Utf8StringCodec::default().encode("".to_string(), dst)?
                    }
                    super::ClientId::IdWithCleanSession(id)
                    | super::ClientId::IdWithExistingSession(id) => {
                        super::Utf8StringCodec::default().encode(id, dst)?
                    }
                }

                if let Some(will) = will {
                    super::Utf8StringCodec::default().encode(will.topic_name, dst)?;
                    #[allow(clippy::cast_possible_truncation)]
                    let will_len = match will.payload.len() {
                        will_len if will_len <= u16::max_value() as usize => will_len as u16,
                        will_len => return Err(super::EncodeError::WillTooLarge(will_len)),
                    };
                    dst.append_u16_be(will_len);
                    dst.extend_from_slice(&will.payload);
                }

                if let Some(username) = username {
                    super::Utf8StringCodec::default().encode(username, dst)?;
                }

                if let Some(password) = password {
                    super::Utf8StringCodec::default().encode(password, dst)?;
                }

                Ok(())
            })?,

            Packet::Disconnect => encode_packet(dst, Packet::DISCONNECT, |_| Ok(()))?,

            Packet::PingReq => encode_packet(dst, Packet::PINGREQ, |_| Ok(()))?,

            Packet::PingResp => encode_packet(dst, Packet::PINGRESP, |_| Ok(()))?,

            Packet::PubAck { packet_identifier } => encode_packet(dst, Packet::PUBACK, |dst| {
                dst.append_packet_identifier(packet_identifier);
                Ok(())
            })?,

            Packet::PubComp { packet_identifier } => encode_packet(dst, Packet::PUBCOMP, |dst| {
                dst.append_packet_identifier(packet_identifier);
                Ok(())
            })?,

            Packet::Publish {
                packet_identifier_dup_qos,
                retain,
                topic_name,
                payload,
            } => {
                let mut packet_type = Packet::PUBLISH;
                packet_type |= match packet_identifier_dup_qos {
                    PacketIdentifierDupQoS::AtMostOnce => 0x00,
                    PacketIdentifierDupQoS::AtLeastOnce(_, true) => 0x0A,
                    PacketIdentifierDupQoS::AtLeastOnce(_, false) => 0x02,
                    PacketIdentifierDupQoS::ExactlyOnce(_, true) => 0x0C,
                    PacketIdentifierDupQoS::ExactlyOnce(_, false) => 0x04,
                };
                if retain {
                    packet_type |= 0x01;
                }

                encode_packet(dst, packet_type, |dst| {
                    super::Utf8StringCodec::default().encode(topic_name, dst)?;

                    match packet_identifier_dup_qos {
                        PacketIdentifierDupQoS::AtMostOnce => (),
                        PacketIdentifierDupQoS::AtLeastOnce(packet_identifier, _)
                        | PacketIdentifierDupQoS::ExactlyOnce(packet_identifier, _) => {
                            dst.append_packet_identifier(packet_identifier)
                        }
                    }

                    dst.extend_from_slice(&payload);

                    Ok(())
                })?;
            }

            Packet::PubRec { packet_identifier } => encode_packet(dst, Packet::PUBREC, |dst| {
                dst.append_packet_identifier(packet_identifier);
                Ok(())
            })?,

            Packet::PubRel { packet_identifier } => {
                encode_packet(dst, Packet::PUBREL | 0x02, |dst| {
                    dst.append_packet_identifier(packet_identifier);
                    Ok(())
                })?
            }

            Packet::SubAck {
                packet_identifier,
                qos,
            } => encode_packet(dst, Packet::SUBACK, |dst| {
                dst.append_packet_identifier(packet_identifier);

                for qos in qos {
                    dst.append_u8(qos.into());
                }

                Ok(())
            })?,

            Packet::Subscribe {
                packet_identifier,
                subscribe_to,
            } => encode_packet(dst, Packet::SUBSCRIBE | 0x02, |dst| {
                dst.append_packet_identifier(packet_identifier);

                for SubscribeTo { topic_filter, qos } in subscribe_to {
                    super::Utf8StringCodec::default().encode(topic_filter, dst)?;
                    dst.append_u8(qos.into());
                }

                Ok(())
            })?,

            Packet::UnsubAck { packet_identifier } => {
                encode_packet(dst, Packet::UNSUBACK, |dst| {
                    dst.append_packet_identifier(packet_identifier);
                    Ok(())
                })?
            }

            Packet::Unsubscribe {
                packet_identifier,
                unsubscribe_from,
            } => encode_packet(dst, Packet::UNSUBSCRIBE | 0x02, |dst| {
                dst.append_packet_identifier(packet_identifier);

                for unsubscribe_from in unsubscribe_from {
                    super::Utf8StringCodec::default().encode(unsubscribe_from, dst)?;
                }

                Ok(())
            })?,
        }

        Ok(())
    }
}

fn encode_packet<F>(
    dst: &mut bytes::BytesMut,
    packet_type: u8,
    f: F,
) -> Result<(), super::EncodeError>
where
    F: FnOnce(&mut bytes::BytesMut) -> Result<(), super::EncodeError>,
{
    use tokio::codec::Encoder;

    let mut remaining_dst = bytes::BytesMut::new();
    f(&mut remaining_dst)?;

    dst.append_u8(packet_type);
    super::RemainingLengthCodec::default().encode(remaining_dst.len(), dst)?;
    dst.extend_from_slice(&remaining_dst);

    Ok(())
}
