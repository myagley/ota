use futures::{Future, Sink, Stream};

#[derive(Debug)]
pub(super) struct State {
    subscriptions: std::collections::HashMap<String, crate::proto::QoS>,

    subscriptions_updated_send: futures::sync::mpsc::Sender<SubscriptionUpdate>,
    subscriptions_updated_recv: futures::sync::mpsc::Receiver<SubscriptionUpdate>,

    subscription_updates_waiting_to_be_sent: std::collections::VecDeque<SubscriptionUpdate>,
    subscription_updates_waiting_to_be_acked:
        std::collections::VecDeque<(crate::proto::PacketIdentifier, BatchedSubscriptionUpdate)>,
}

impl State {
    pub(super) fn poll(
        &mut self,
        packet: &mut Option<crate::proto::Packet>,
        packet_identifiers: &mut super::PacketIdentifiers,
    ) -> Result<(Vec<crate::proto::Packet>, Vec<SubscriptionUpdate>), super::Error> {
        let mut subscription_updates = vec![];

        match packet.take() {
            Some(crate::proto::Packet::SubAck {
                packet_identifier,
                qos,
            }) => match self.subscription_updates_waiting_to_be_acked.pop_front() {
                Some((
                    packet_identifier_waiting_to_be_acked,
                    BatchedSubscriptionUpdate::Subscribe(subscribe_to),
                )) => {
                    if packet_identifier != packet_identifier_waiting_to_be_acked {
                        self.subscription_updates_waiting_to_be_acked.push_front((
                            packet_identifier_waiting_to_be_acked,
                            BatchedSubscriptionUpdate::Subscribe(subscribe_to),
                        ));
                        return Err(super::Error::UnexpectedSubAck(
                            packet_identifier,
                            super::UnexpectedSubUnsubAckReason::Expected(
                                packet_identifier_waiting_to_be_acked,
                            ),
                        ));
                    }

                    if subscribe_to.len() != qos.len() {
                        let expected = subscribe_to.len();
                        self.subscription_updates_waiting_to_be_acked.push_front((
                            packet_identifier_waiting_to_be_acked,
                            BatchedSubscriptionUpdate::Subscribe(subscribe_to),
                        ));
                        return Err(super::Error::SubAckDoesNotContainEnoughQoS(
                            packet_identifier,
                            expected,
                            qos.len(),
                        ));
                    }

                    packet_identifiers.discard(packet_identifier);

                    // We can't put subscribe_to back into self.subscription_updates_waiting_to_be_acked within the below loop
                    // since we would've partially consumed it.
                    // Instead, if there's an error, we'll update self.subscriptions anyway with the expected QoS, and set the error to be returned here.
                    // The error will reset the session and resend the subscription requests, including these that didn't match the expected QoS,
                    // so pretending the subscription succeeded does no harm.
                    let mut err = None;
                    for (
                        crate::proto::SubscribeTo {
                            topic_filter,
                            qos: expected_qos,
                        },
                        qos,
                    ) in subscribe_to.into_iter().zip(qos)
                    {
                        match qos {
                            crate::proto::SubAckQos::Success(actual_qos) => {
                                if actual_qos >= expected_qos {
                                    log::debug!(
                                        "Subscribed to {} with {:?}",
                                        topic_filter,
                                        actual_qos
                                    );
                                    self.subscriptions.insert(topic_filter.clone(), actual_qos);
                                    subscription_updates.push(SubscriptionUpdate::Subscribe(
                                        crate::proto::SubscribeTo {
                                            topic_filter,
                                            qos: actual_qos,
                                        },
                                    ));
                                } else {
                                    if err.is_none() {
                                        err = Some(super::Error::SubscriptionDowngraded(
                                            topic_filter.clone(),
                                            expected_qos,
                                            actual_qos,
                                        ));
                                    }

                                    self.subscriptions.insert(topic_filter, expected_qos);
                                }
                            }

                            crate::proto::SubAckQos::Failure => {
                                if err.is_none() {
                                    err = Some(super::Error::SubscriptionRejectedByServer);
                                }

                                self.subscriptions.insert(topic_filter, expected_qos);
                            }
                        }
                    }

                    if let Some(err) = err {
                        return Err(err);
                    }
                }

                Some((
                    packet_identifier_waiting_to_be_acked,
                    unsubscribe @ BatchedSubscriptionUpdate::Unsubscribe(_),
                )) => {
                    self.subscription_updates_waiting_to_be_acked
                        .push_front((packet_identifier, unsubscribe));
                    return Err(super::Error::UnexpectedSubAck(
                        packet_identifier,
                        super::UnexpectedSubUnsubAckReason::ExpectedUnsubAck(
                            packet_identifier_waiting_to_be_acked,
                        ),
                    ));
                }

                None => {
                    return Err(super::Error::UnexpectedSubAck(
                        packet_identifier,
                        super::UnexpectedSubUnsubAckReason::DidNotExpect,
                    ));
                }
            },

            Some(crate::proto::Packet::UnsubAck { packet_identifier }) => {
                match self.subscription_updates_waiting_to_be_acked.pop_front() {
                    Some((
                        packet_identifier_waiting_to_be_acked,
                        BatchedSubscriptionUpdate::Unsubscribe(unsubscribe_from),
                    )) => {
                        if packet_identifier != packet_identifier_waiting_to_be_acked {
                            self.subscription_updates_waiting_to_be_acked.push_front((
                                packet_identifier_waiting_to_be_acked,
                                BatchedSubscriptionUpdate::Unsubscribe(unsubscribe_from),
                            ));
                            return Err(super::Error::UnexpectedUnsubAck(
                                packet_identifier,
                                super::UnexpectedSubUnsubAckReason::Expected(
                                    packet_identifier_waiting_to_be_acked,
                                ),
                            ));
                        }

                        packet_identifiers.discard(packet_identifier);

                        for topic_filter in unsubscribe_from {
                            log::debug!("Unsubscribed from {}", topic_filter);
                            self.subscriptions.remove(&topic_filter);
                            subscription_updates
                                .push(SubscriptionUpdate::Unsubscribe(topic_filter));
                        }
                    }

                    Some((
                        packet_identifier_waiting_to_be_acked,
                        subscribe @ BatchedSubscriptionUpdate::Subscribe(_),
                    )) => {
                        self.subscription_updates_waiting_to_be_acked
                            .push_front((packet_identifier_waiting_to_be_acked, subscribe));
                        return Err(super::Error::UnexpectedUnsubAck(
                            packet_identifier,
                            super::UnexpectedSubUnsubAckReason::ExpectedSubAck(
                                packet_identifier_waiting_to_be_acked,
                            ),
                        ));
                    }

                    None => {
                        return Err(super::Error::UnexpectedUnsubAck(
                            packet_identifier,
                            super::UnexpectedSubUnsubAckReason::DidNotExpect,
                        ));
                    }
                }
            }

            other => *packet = other,
        }

        while let futures::Async::Ready(Some(subscription_to_update)) = self
            .subscriptions_updated_recv
            .poll()
            .expect("Receiver::poll cannot fail")
        {
            self.subscription_updates_waiting_to_be_sent
                .push_back(subscription_to_update);
        }

        let mut packets_waiting_to_be_sent = vec![];

        if !self.subscription_updates_waiting_to_be_sent.is_empty() {
            // Rather than send individual SUBSCRIBE and UNSUBSCRIBE packets for each update, we can send multiple updates in the same packet.
            // subscription_updates_waiting_to_be_sent may contain Subscribe and Unsubscribe in arbitrary order, so we have to partition them into
            // a group of Subscribe and a group of Unsubscribe.
            //
            // But the client have have unsubscribed to an earlier subscription, and both the Subscribe and the later Unsubscribe might be in this list.
            // Similarly, the client have have re-subscribed after unsubscribing, and both the Unsubscribe and the later Subscribe might be in this list.
            //
            // So we cannot just make a group of all Subscribes, send that packet, then make a group of all Unsubscribes, then send that packet.
            // Instead, we have to respect the ordering of Subscribes with Unsubscribes.
            // So we make an intermediate set of all subscriptions based on the updates waiting to be sent, compute the diff from the current subscriptions,
            // then send a SUBSCRIBE packet for any net new subscriptions and an UNSUBSCRIBE packet for any net new unsubscriptions.

            let mut target_subscriptions = self.subscriptions.clone();

            while let Some(subscription_update) =
                self.subscription_updates_waiting_to_be_sent.pop_front()
            {
                match subscription_update {
                    SubscriptionUpdate::Subscribe(subscribe_to) => {
                        target_subscriptions.insert(subscribe_to.topic_filter, subscribe_to.qos)
                    }
                    SubscriptionUpdate::Unsubscribe(unsubscribe_from) => {
                        target_subscriptions.remove(&unsubscribe_from)
                    }
                };
            }

            let mut pending_subscriptions = vec![];
            for (topic_filter, &qos) in &target_subscriptions {
                if self.subscriptions.get(topic_filter) != Some(&qos) {
                    // Current subscription doesn't exist, or exists but has different QoS
                    pending_subscriptions.push(crate::proto::SubscribeTo {
                        topic_filter: topic_filter.clone(),
                        qos,
                    });
                }
            }
            pending_subscriptions.sort_by(|s1, s2| s1.topic_filter.cmp(&s2.topic_filter));

            let mut pending_unsubscriptions = vec![];
            for topic_filter in self.subscriptions.keys() {
                if !target_subscriptions.contains_key(topic_filter) {
                    pending_unsubscriptions.push(topic_filter.clone());
                }
            }
            pending_unsubscriptions.sort();

            // Save the error, if any, from reserving a packet identifier
            // This error is only returned if neither subscription nor unsubscription generated a packet to send
            // This avoids having to discard a valid packet identifier for a SUBSCRIBE packet just because
            // the unsubscription failed to reserve a packet identifier for an UNSUBSCRIBE packet.
            let mut err = None;

            if !pending_subscriptions.is_empty() {
                match packet_identifiers.reserve() {
                    Ok(packet_identifier) => {
                        self.subscription_updates_waiting_to_be_acked.push_back((
                            packet_identifier,
                            BatchedSubscriptionUpdate::Subscribe(pending_subscriptions.clone()),
                        ));

                        packets_waiting_to_be_sent.push(crate::proto::Packet::Subscribe {
                            packet_identifier,
                            subscribe_to: pending_subscriptions,
                        });
                    }

                    Err(err_) => {
                        err = Some(err_);

                        for pending_subscription in pending_subscriptions {
                            self.subscription_updates_waiting_to_be_sent
                                .push_front(SubscriptionUpdate::Subscribe(pending_subscription));
                        }
                    }
                };
            }

            if !pending_unsubscriptions.is_empty() {
                match packet_identifiers.reserve() {
                    Ok(packet_identifier) => {
                        self.subscription_updates_waiting_to_be_acked.push_back((
                            packet_identifier,
                            BatchedSubscriptionUpdate::Unsubscribe(pending_unsubscriptions.clone()),
                        ));

                        packets_waiting_to_be_sent.push(crate::proto::Packet::Unsubscribe {
                            packet_identifier,
                            unsubscribe_from: pending_unsubscriptions,
                        });
                    }

                    Err(err_) => {
                        err = Some(err_);

                        for pending_unsubscription in pending_unsubscriptions {
                            self.subscription_updates_waiting_to_be_sent.push_front(
                                SubscriptionUpdate::Unsubscribe(pending_unsubscription),
                            );
                        }
                    }
                };
            }

            if packets_waiting_to_be_sent.is_empty() {
                if let Some(err) = err {
                    return Err(err);
                }
            }
        }

        Ok((packets_waiting_to_be_sent, subscription_updates))
    }

    pub(super) fn new_connection(
        &mut self,
        reset_session: bool,
        packet_identifiers: &mut super::PacketIdentifiers,
    ) -> impl Iterator<Item = crate::proto::Packet> {
        if reset_session {
            let mut subscriptions = std::mem::replace(&mut self.subscriptions, Default::default());
            let subscription_updates_waiting_to_be_acked = std::mem::replace(
                &mut self.subscription_updates_waiting_to_be_acked,
                Default::default(),
            );

            // Apply all pending (ie unacked) changes to the set of subscriptions, in order that they were original requested
            for (packet_identifier, subscription_update_waiting_to_be_acked) in
                subscription_updates_waiting_to_be_acked
            {
                packet_identifiers.discard(packet_identifier);

                match subscription_update_waiting_to_be_acked {
                    BatchedSubscriptionUpdate::Subscribe(subscribe_to) => {
                        for crate::proto::SubscribeTo { topic_filter, qos } in subscribe_to {
                            subscriptions.insert(topic_filter, qos);
                        }
                    }

                    BatchedSubscriptionUpdate::Unsubscribe(unsubscribe_from) => {
                        for topic_filter in unsubscribe_from {
                            subscriptions.remove(&topic_filter);
                        }
                    }
                }
            }

            // Generate a SUBSCRIBE packet for the final set of subscriptions
            let mut subscriptions_waiting_to_be_acked: Vec<_> = subscriptions
                .into_iter()
                .map(|(topic_filter, qos)| crate::proto::SubscribeTo { topic_filter, qos })
                .collect();
            subscriptions_waiting_to_be_acked.sort_by(|subscribe_to1, subscribe_to2| {
                subscribe_to1.topic_filter.cmp(&subscribe_to2.topic_filter)
            });

            if subscriptions_waiting_to_be_acked.is_empty() {
                NewConnectionIter::Empty
            } else {
                let packet_identifier = packet_identifiers
                    .reserve()
                    .expect("reset session should have available packet identifiers");
                self.subscription_updates_waiting_to_be_acked.push_back((
                    packet_identifier,
                    BatchedSubscriptionUpdate::Subscribe(subscriptions_waiting_to_be_acked.clone()),
                ));

                NewConnectionIter::Single(std::iter::once(crate::proto::Packet::Subscribe {
                    packet_identifier,
                    subscribe_to: subscriptions_waiting_to_be_acked,
                }))
            }
        } else {
            // Re-create all pending (ie unacked) changes to the set of subscriptions
            let unacked_packets: Vec<_> = self
                .subscription_updates_waiting_to_be_acked
                .iter()
                .map(
                    |(packet_identifier, subscription_update)| match subscription_update {
                        BatchedSubscriptionUpdate::Subscribe(subscribe_to) => {
                            crate::proto::Packet::Subscribe {
                                packet_identifier: *packet_identifier,
                                subscribe_to: subscribe_to.clone(),
                            }
                        }

                        BatchedSubscriptionUpdate::Unsubscribe(unsubscribe_from) => {
                            crate::proto::Packet::Unsubscribe {
                                packet_identifier: *packet_identifier,
                                unsubscribe_from: unsubscribe_from.clone(),
                            }
                        }
                    },
                )
                .collect();

            NewConnectionIter::Multiple(unacked_packets.into_iter())
        }
    }

    pub(super) fn update_subscription(&mut self, subscription_update: SubscriptionUpdate) {
        self.subscription_updates_waiting_to_be_sent
            .push_back(subscription_update);
    }

    pub(super) fn update_subscription_handle(&self) -> UpdateSubscriptionHandle {
        UpdateSubscriptionHandle(self.subscriptions_updated_send.clone())
    }
}

impl Default for State {
    fn default() -> Self {
        let (subscriptions_updated_send, subscriptions_updated_recv) =
            futures::sync::mpsc::channel(0);

        State {
            subscriptions: Default::default(),

            subscriptions_updated_send,
            subscriptions_updated_recv,

            subscription_updates_waiting_to_be_sent: Default::default(),
            subscription_updates_waiting_to_be_acked: Default::default(),
        }
    }
}

/// The kind of subscription update
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubscriptionUpdate {
    Subscribe(crate::proto::SubscribeTo),
    Unsubscribe(String),
}

#[derive(Debug)]
enum BatchedSubscriptionUpdate {
    Subscribe(Vec<crate::proto::SubscribeTo>),
    Unsubscribe(Vec<String>),
}

#[derive(Debug)]
enum NewConnectionIter {
    Empty,
    Single(std::iter::Once<crate::proto::Packet>),
    Multiple(std::vec::IntoIter<crate::proto::Packet>),
}

impl Iterator for NewConnectionIter {
    type Item = crate::proto::Packet;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            NewConnectionIter::Empty => None,
            NewConnectionIter::Single(packet) => packet.next(),
            NewConnectionIter::Multiple(packets) => packets.next(),
        }
    }
}

/// Used to update subscriptions
pub struct UpdateSubscriptionHandle(futures::sync::mpsc::Sender<SubscriptionUpdate>);

impl UpdateSubscriptionHandle {
    /// Subscribe to a topic with the given parameters.
    ///
    /// The [`Future`] returned by this function resolves when the subscription update is received by the client.
    /// The client has *not necessarily* sent out the subscription update to the server at that point,
    /// and the server has *not necessarily* acked the subscription update at that point.
    ///
    /// This is done because the client automatically resubscribes when the connection is broken and re-established, so the user
    /// of the client needs to know about this every time the server acks the subscription, not just the first time they request it.
    ///
    /// Furthermore, the client batches subscription updates, which can cause some subscription updates to never be sent (say because a subscription
    /// was canceled out by a matching unsubscription before the subscription was ever sent to the server). So there is not a one-to-one correspondence
    /// between subscription update requests and acks.
    ///
    /// To know when the server has acked the subscription update, wait for the client to send an [`mqtt::Event::SubscriptionUpdate::Subscribe`] value
    /// that contains a `mqtt::proto::SubscribeTo` value with the same topic filter.
    /// Be careful about using `==` to determine this, since the QoS in the event may be higher than the one requested here.
    pub fn subscribe(
        &mut self,
        subscribe_to: crate::proto::SubscribeTo,
    ) -> impl Future<Item = (), Error = UpdateSubscriptionError> {
        self.0
            .clone()
            .send(SubscriptionUpdate::Subscribe(subscribe_to))
            .then(|result| match result {
                Ok(_) => Ok(()),
                Err(_) => Err(UpdateSubscriptionError::ClientDoesNotExist),
            })
    }

    /// Unsubscribe from the given topic.
    ///
    /// The [`Future`] returned by this function resolves when the subscription update is received by the client.
    /// The client has *not necessarily* sent out the subscription update to the server at that point,
    /// and the server has *not necessarily* acked the subscription update at that point.
    ///
    /// This is done because the client automatically resubscribes when the connection is broken and re-established, so the user
    /// of the client needs to know about this every time the server acks the subscription, not just the first time they request it.
    ///
    /// Furthermore, the client batches subscription updates, which can cause some subscription updates to never be sent (say because a subscription
    /// was canceled out by a matching unsubscription before the subscription was ever sent to the server). So there is not a one-to-one correspondence
    /// between subscription update requests and acks.
    ///
    /// To know when the server has acked the subscription update, wait for the client to send an [`mqtt::Event::SubscriptionUpdate::Unsubscribe`] value
    /// for this topic filter.
    pub fn unsubscribe(
        &mut self,
        unsubscribe_from: String,
    ) -> impl Future<Item = (), Error = UpdateSubscriptionError> {
        self.0
            .clone()
            .send(SubscriptionUpdate::Unsubscribe(unsubscribe_from))
            .then(|result| match result {
                Ok(_) => Ok(()),
                Err(_) => Err(UpdateSubscriptionError::ClientDoesNotExist),
            })
    }
}

#[derive(Clone, Copy, Debug)]
pub enum UpdateSubscriptionError {
    ClientDoesNotExist,
}

impl std::fmt::Display for UpdateSubscriptionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpdateSubscriptionError::ClientDoesNotExist => write!(f, "client does not exist"),
        }
    }
}

impl std::error::Error for UpdateSubscriptionError {}
