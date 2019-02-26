use futures::{Future, Sink, Stream};

#[derive(Debug)]
pub(super) struct State {
    publish_request_send: futures::sync::mpsc::Sender<PublishRequest>,
    publish_request_recv: futures::sync::mpsc::Receiver<PublishRequest>,

    publish_requests_waiting_to_be_sent: std::collections::VecDeque<PublishRequest>,

    /// Holds PUBLISH packets sent by us, waiting for a corresponding PUBACK or PUBREC
    waiting_to_be_acked: std::collections::BTreeMap<
        crate::proto::PacketIdentifier,
        (futures::sync::oneshot::Sender<()>, crate::proto::Packet),
    >,

    /// Holds the identifiers of PUBREC packets sent by us, waiting for a corresponding PUBREL,
    /// and the contents of the original PUBLISH packet for which we sent the PUBREC
    waiting_to_be_released:
        std::collections::BTreeMap<crate::proto::PacketIdentifier, crate::ReceivedPublication>,

    /// Holds PUBLISH packets sent by us, waiting for a corresponding PUBCOMP
    waiting_to_be_completed: std::collections::BTreeMap<
        crate::proto::PacketIdentifier,
        (futures::sync::oneshot::Sender<()>, crate::proto::Packet),
    >,
}

impl State {
    pub(super) fn poll(
        &mut self,
        packet: &mut Option<crate::proto::Packet>,
        packet_identifiers: &mut super::PacketIdentifiers,
    ) -> Result<
        (
            Vec<crate::proto::Packet>,
            Option<crate::ReceivedPublication>,
        ),
        super::Error,
    > {
        let mut packets_waiting_to_be_sent = vec![];
        let mut publication_received = None;

        match packet.take() {
            Some(crate::proto::Packet::PubAck { packet_identifier }) => {
                match self.waiting_to_be_acked.remove(&packet_identifier) {
                    Some((ack_sender, _)) => {
                        packet_identifiers.discard(packet_identifier);

                        match ack_sender.send(()) {
						Ok(()) => (),
						Err(()) => log::debug!("could not send ack for publish request because ack receiver has been dropped"),
					}
                    }
                    None => log::warn!("ignoring PUBACK for a PUBLISH we never sent"),
                }
            }

            Some(crate::proto::Packet::PubComp { packet_identifier }) => {
                match self.waiting_to_be_completed.remove(&packet_identifier) {
                    Some((ack_sender, _)) => {
                        packet_identifiers.discard(packet_identifier);

                        match ack_sender.send(()) {
						Ok(()) => (),
						Err(()) => log::debug!("could not send ack for publish request because ack receiver has been dropped"),
					}
                    }
                    None => log::warn!("ignoring PUBCOMP for a PUBREL we never sent"),
                }
            }

            Some(crate::proto::Packet::Publish {
                packet_identifier_dup_qos,
                retain,
                topic_name,
                payload,
            }) => match packet_identifier_dup_qos {
                crate::proto::PacketIdentifierDupQoS::AtMostOnce => {
                    publication_received = Some(crate::ReceivedPublication {
                        topic_name,
                        dup: false,
                        qos: crate::proto::QoS::AtMostOnce,
                        retain,
                        payload,
                    });
                }

                crate::proto::PacketIdentifierDupQoS::AtLeastOnce(packet_identifier, dup) => {
                    publication_received = Some(crate::ReceivedPublication {
                        topic_name,
                        dup,
                        qos: crate::proto::QoS::AtLeastOnce,
                        retain,
                        payload,
                    });

                    packets_waiting_to_be_sent
                        .push(crate::proto::Packet::PubAck { packet_identifier });
                }

                crate::proto::PacketIdentifierDupQoS::ExactlyOnce(packet_identifier, dup) => {
                    match self.waiting_to_be_released.entry(packet_identifier) {
                        std::collections::btree_map::Entry::Occupied(_) =>
                        // This PUBLISH was already received earlier and a PUBREC sent in response, but the server apparently didn't receive it.
                        // Send another PUBREC and ignore this PUBLISH.
                        {
                            if !dup {
                                return Err(super::Error::DuplicateExactlyOncePublishPacketNotMarkedDuplicate(packet_identifier));
                            }
                        }

                        std::collections::btree_map::Entry::Vacant(entry) => {
                            // ExactlyOnce publications should only be sent to the client when the corresponding PUBREL is received.
                            // Otherwise the server might send the PUBLISH again after a session reset and we would have no way of knowing we should ignore it.
                            entry.insert(crate::ReceivedPublication {
                                topic_name,
                                dup,
                                qos: crate::proto::QoS::ExactlyOnce,
                                retain,
                                payload,
                            });
                        }
                    }

                    packets_waiting_to_be_sent
                        .push(crate::proto::Packet::PubRec { packet_identifier });
                }
            },

            Some(crate::proto::Packet::PubRec { packet_identifier }) => {
                match self.waiting_to_be_acked.remove(&packet_identifier) {
                    Some((ack_sender, packet)) => {
                        self.waiting_to_be_completed
                            .insert(packet_identifier, (ack_sender, packet));
                    }
                    None => log::warn!("ignoring PUBREC for a PUBLISH we never sent"),
                }

                packets_waiting_to_be_sent.push(crate::proto::Packet::PubRel { packet_identifier });
            }

            Some(crate::proto::Packet::PubRel { packet_identifier }) => {
                if let Some(publication) = self.waiting_to_be_released.remove(&packet_identifier) {
                    packet_identifiers.discard(packet_identifier);
                    publication_received = Some(publication);
                } else {
                    log::warn!("ignoring PUBREL for a PUBREC we never sent");
                }

                packets_waiting_to_be_sent
                    .push(crate::proto::Packet::PubComp { packet_identifier });
            }

            other => *packet = other,
        }

        while let futures::Async::Ready(Some(publish_request)) = self
            .publish_request_recv
            .poll()
            .expect("Receiver::poll cannot fail")
        {
            self.publish_requests_waiting_to_be_sent
                .push_back(publish_request);
        }

        while let Some(PublishRequest {
            publication,
            ack_sender,
        }) = self.publish_requests_waiting_to_be_sent.pop_front()
        {
            match publication.qos {
                crate::proto::QoS::AtMostOnce => {
                    packets_waiting_to_be_sent.push(crate::proto::Packet::Publish {
                        packet_identifier_dup_qos: crate::proto::PacketIdentifierDupQoS::AtMostOnce,
                        retain: publication.retain,
                        topic_name: publication.topic_name,
                        payload: publication.payload,
                    });

                    match ack_sender.send(()) {
						Ok(()) => (),
						Err(()) => log::debug!("could not send ack for publish request because ack receiver has been dropped"),
					}
                }

                crate::proto::QoS::AtLeastOnce => {
                    let packet_identifier = match packet_identifiers.reserve() {
                        Ok(packet_identifier) => packet_identifier,
                        Err(err) => {
                            self.publish_requests_waiting_to_be_sent
                                .push_front(PublishRequest {
                                    publication,
                                    ack_sender,
                                });
                            return Err(err);
                        }
                    };

                    let packet = crate::proto::Packet::Publish {
                        packet_identifier_dup_qos:
                            crate::proto::PacketIdentifierDupQoS::AtLeastOnce(
                                packet_identifier,
                                false,
                            ),
                        retain: publication.retain,
                        topic_name: publication.topic_name.clone(),
                        payload: publication.payload.clone(),
                    };

                    self.waiting_to_be_acked.insert(
                        packet_identifier,
                        (
                            ack_sender,
                            crate::proto::Packet::Publish {
                                packet_identifier_dup_qos:
                                    crate::proto::PacketIdentifierDupQoS::AtLeastOnce(
                                        packet_identifier,
                                        true,
                                    ),
                                retain: publication.retain,
                                topic_name: publication.topic_name,
                                payload: publication.payload,
                            },
                        ),
                    );

                    packets_waiting_to_be_sent.push(packet);
                }

                crate::proto::QoS::ExactlyOnce => {
                    let packet_identifier = match packet_identifiers.reserve() {
                        Ok(packet_identifier) => packet_identifier,
                        Err(err) => {
                            self.publish_requests_waiting_to_be_sent
                                .push_front(PublishRequest {
                                    publication,
                                    ack_sender,
                                });
                            return Err(err);
                        }
                    };

                    let packet = crate::proto::Packet::Publish {
                        packet_identifier_dup_qos:
                            crate::proto::PacketIdentifierDupQoS::ExactlyOnce(
                                packet_identifier,
                                false,
                            ),
                        retain: publication.retain,
                        topic_name: publication.topic_name.clone(),
                        payload: publication.payload.clone(),
                    };

                    self.waiting_to_be_acked.insert(
                        packet_identifier,
                        (
                            ack_sender,
                            crate::proto::Packet::Publish {
                                packet_identifier_dup_qos:
                                    crate::proto::PacketIdentifierDupQoS::ExactlyOnce(
                                        packet_identifier,
                                        true,
                                    ),
                                retain: publication.retain,
                                topic_name: publication.topic_name,
                                payload: publication.payload,
                            },
                        ),
                    );

                    packets_waiting_to_be_sent.push(packet);
                }
            }
        }

        Ok((packets_waiting_to_be_sent, publication_received))
    }

    pub(super) fn new_connection<'a>(
        &'a mut self,
        reset_session: bool,
        packet_identifiers: &mut super::PacketIdentifiers,
    ) -> impl Iterator<Item = crate::proto::Packet> + 'a {
        if reset_session {
            // Move all waiting_to_be_completed back to waiting_to_be_acked since we must restart the ExactlyOnce protocol flow
            self.waiting_to_be_acked
                .append(&mut self.waiting_to_be_completed);

            // Clear waiting_to_be_released
            for (packet_identifier, _) in
                std::mem::replace(&mut self.waiting_to_be_released, Default::default())
            {
                packet_identifiers.discard(packet_identifier);
            }
        }

        self.waiting_to_be_acked
            .values()
            .map(|(_, packet)| packet.clone())
            .chain(
                self.waiting_to_be_released
                    .keys()
                    .map(|&packet_identifier| crate::proto::Packet::PubRec { packet_identifier }),
            )
            .chain(
                self.waiting_to_be_completed
                    .values()
                    .map(|(_, packet)| packet.clone()),
            )
    }

    pub(super) fn publish(
        &mut self,
        publication: crate::proto::Publication,
    ) -> impl Future<Item = (), Error = PublishError> {
        let (ack_sender, ack_receiver) = futures::sync::oneshot::channel();
        self.publish_requests_waiting_to_be_sent
            .push_back(PublishRequest {
                publication,
                ack_sender,
            });
        ack_receiver.map_err(|_| PublishError::ClientDoesNotExist)
    }

    pub(super) fn publish_handle(&self) -> PublishHandle {
        PublishHandle(self.publish_request_send.clone())
    }
}

impl Default for State {
    fn default() -> Self {
        let (publish_request_send, publish_request_recv) = futures::sync::mpsc::channel(0);

        State {
            publish_request_send,
            publish_request_recv,

            publish_requests_waiting_to_be_sent: Default::default(),
            waiting_to_be_acked: Default::default(),
            waiting_to_be_released: Default::default(),
            waiting_to_be_completed: Default::default(),
        }
    }
}

/// Used to publish messages to the server
pub struct PublishHandle(futures::sync::mpsc::Sender<PublishRequest>);

impl PublishHandle {
    /// Publish the given message to the server
    pub fn publish(
        &mut self,
        publication: crate::proto::Publication,
    ) -> impl Future<Item = (), Error = PublishError> {
        let (ack_sender, ack_receiver) = futures::sync::oneshot::channel();

        self.0
            .clone()
            .send(PublishRequest {
                publication,
                ack_sender,
            })
            .then(|result| match result {
                Ok(_) => Ok(ack_receiver.map_err(|_| PublishError::ClientDoesNotExist)),
                Err(_) => Err(PublishError::ClientDoesNotExist),
            })
            .flatten()
    }
}

#[derive(Debug)]
pub enum PublishError {
    ClientDoesNotExist,
}

impl std::fmt::Display for PublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PublishError::ClientDoesNotExist => write!(f, "client does not exist"),
        }
    }
}

impl std::error::Error for PublishError {}

#[derive(Debug)]
struct PublishRequest {
    publication: crate::proto::Publication,
    ack_sender: futures::sync::oneshot::Sender<()>,
}
