use futures::{Future, Sink, Stream};

mod connect;
mod ping;
mod publish;
mod subscriptions;

pub use self::publish::{PublishError, PublishHandle};
pub use self::subscriptions::{
    SubscriptionUpdate, UpdateSubscriptionError, UpdateSubscriptionHandle,
};

/// An MQTT v3.1.1 client.
///
/// A `Client` is a [`Stream`] of [`Event`]s. It automatically reconnects if the connection to the server is broken,
/// and handles session state.
///
/// Publish messages to the server using the handle returned by [`Client::publish_handle`].
///
/// Subscribe to and unsubscribe from topics using the handle returned by [`Client::update_subscription_handle`].
///
/// The [`Stream`] only ends (returns `Ready(None)`) when the client is told to shut down gracefully using the handle
/// returned by [`Client::shutdown_handle`]. The `Client` becomes unusable after it has returned `None`
/// and should be dropped.
#[derive(Debug)]
pub struct Client<IoS>(ClientState<IoS>)
where
    IoS: IoSource;

impl<IoS> Client<IoS>
where
    IoS: IoSource,
{
    /// Create a new client with the given parameters
    ///
    /// * `client_id`
    ///
    ///     If set, this ID will be used to start a new clean session with the server. On subsequent re-connects, the ID will be re-used.
    ///     Otherwise, the client will use a server-generated ID for each new connection.
    ///
    /// * `username`, `password`
    ///
    ///     Optional credentials for the server.
    ///
    /// * `io_source`
    ///
    ///     The MQTT protocol is layered onto the I/O object returned by this source.
    ///
    /// * `max_reconnect_back_off`
    ///
    ///     Every connection failure will double the back-off period, to a maximum of this value.
    ///
    /// * `keep_alive`
    ///
    ///     The keep-alive time advertised to the server. The client will ping the server at half this interval.
    pub fn new(
        client_id: Option<String>,
        username: Option<String>,
        password: Option<String>,
        will: Option<crate::proto::Publication>,
        io_source: IoS,
        max_reconnect_back_off: std::time::Duration,
        keep_alive: std::time::Duration,
    ) -> Self {
        let client_id = match client_id {
            Some(id) => crate::proto::ClientId::IdWithCleanSession(id),
            None => crate::proto::ClientId::ServerGenerated,
        };

        let (shutdown_send, shutdown_recv) = futures::sync::mpsc::channel(0);

        Client(ClientState::Up {
            client_id,
            username,
            password,
            will,
            keep_alive,

            shutdown_send,
            shutdown_recv,

            packet_identifiers: Default::default(),

            connect: self::connect::Connect::new(io_source, max_reconnect_back_off),
            ping: self::ping::State::BeginWaitingForNextPing,
            publish: Default::default(),
            subscriptions: Default::default(),

            packets_waiting_to_be_sent: Default::default(),
        })
    }

    /// Queues a message to be published to the server
    pub fn publish(
        &mut self,
        publication: crate::proto::Publication,
    ) -> impl Future<Item = (), Error = PublishError> {
        match &mut self.0 {
            ClientState::Up { publish, .. } => {
                futures::future::Either::A(publish.publish(publication))
            }
            ClientState::ShuttingDown { .. } | ClientState::ShutDown { .. } => {
                futures::future::Either::B(futures::future::err(PublishError::ClientDoesNotExist))
            }
        }
    }

    /// Returns a handle that can be used to publish messages to the server
    pub fn publish_handle(&self) -> Result<PublishHandle, PublishError> {
        match &self.0 {
            ClientState::Up { publish, .. } => Ok(publish.publish_handle()),
            ClientState::ShuttingDown { .. } | ClientState::ShutDown { .. } => {
                Err(PublishError::ClientDoesNotExist)
            }
        }
    }

    /// Subscribes to a topic with the given parameters
    pub fn subscribe(
        &mut self,
        subscribe_to: crate::proto::SubscribeTo,
    ) -> Result<(), UpdateSubscriptionError> {
        match &mut self.0 {
            ClientState::Up { subscriptions, .. } => {
                subscriptions
                    .update_subscription(crate::SubscriptionUpdate::Subscribe(subscribe_to));
                Ok(())
            }

            ClientState::ShuttingDown { .. } | ClientState::ShutDown { .. } => {
                Err(UpdateSubscriptionError::ClientDoesNotExist)
            }
        }
    }

    /// Unsubscribes from the given topic
    pub fn unsubscribe(&mut self, unsubscribe_from: String) -> Result<(), UpdateSubscriptionError> {
        match &mut self.0 {
            ClientState::Up { subscriptions, .. } => {
                subscriptions
                    .update_subscription(crate::SubscriptionUpdate::Unsubscribe(unsubscribe_from));
                Ok(())
            }

            ClientState::ShuttingDown { .. } | ClientState::ShutDown { .. } => {
                Err(UpdateSubscriptionError::ClientDoesNotExist)
            }
        }
    }

    /// Returns a handle that can be used to update subscriptions
    pub fn update_subscription_handle(
        &self,
    ) -> Result<UpdateSubscriptionHandle, UpdateSubscriptionError> {
        match &self.0 {
            ClientState::Up { subscriptions, .. } => Ok(subscriptions.update_subscription_handle()),
            ClientState::ShuttingDown { .. } | ClientState::ShutDown { .. } => {
                Err(UpdateSubscriptionError::ClientDoesNotExist)
            }
        }
    }

    /// Returns a handle that can be used to signal the client to shut down
    pub fn shutdown_handle(&self) -> Result<ShutdownHandle, ShutdownError> {
        match &self.0 {
            ClientState::Up { shutdown_send, .. } => Ok(ShutdownHandle(shutdown_send.clone())),
            ClientState::ShuttingDown { .. } | ClientState::ShutDown { .. } => {
                Err(ShutdownError::ClientDoesNotExist)
            }
        }
    }
}

impl<IoS> Stream for Client<IoS>
where
    IoS: IoSource,
    <<IoS as IoSource>::Future as Future>::Error: std::fmt::Display,
{
    type Item = Event;
    type Error = Error;

    fn poll(&mut self) -> futures::Poll<Option<Self::Item>, Self::Error> {
        let reason = loop {
            match &mut self.0 {
                ClientState::Up {
                    client_id,
                    username,
                    password,
                    will,
                    keep_alive,

                    shutdown_recv,

                    packet_identifiers,

                    connect,
                    ping,
                    publish,
                    subscriptions,

                    packets_waiting_to_be_sent,
                    ..
                } => {
                    match shutdown_recv.poll().expect("Receiver::poll cannot fail") {
                        futures::Async::Ready(Some(())) => break None,

                        futures::Async::Ready(None) | futures::Async::NotReady => (),
                    }

                    let self::connect::Connected {
                        framed,
                        new_connection,
                        reset_session,
                    } = match connect.poll(
                        username.as_ref().map(AsRef::as_ref),
                        password.as_ref().map(AsRef::as_ref),
                        will.as_ref(),
                        client_id,
                        *keep_alive,
                    ) {
                        Ok(futures::Async::Ready(framed)) => framed,
                        Ok(futures::Async::NotReady) => return Ok(futures::Async::NotReady),
                        Err(()) => unreachable!(),
                    };

                    if new_connection {
                        log::debug!("New connection established");

                        *packets_waiting_to_be_sent = Default::default();

                        ping.new_connection();

                        packets_waiting_to_be_sent
                            .extend(publish.new_connection(reset_session, packet_identifiers));

                        packets_waiting_to_be_sent.extend(
                            subscriptions.new_connection(reset_session, packet_identifiers),
                        );

                        return Ok(futures::Async::Ready(Some(Event::NewConnection {
                            reset_session,
                        })));
                    }

                    match client_poll(
                        framed,
                        *keep_alive,
                        packets_waiting_to_be_sent,
                        packet_identifiers,
                        ping,
                        publish,
                        subscriptions,
                    ) {
                        Ok(futures::Async::Ready(event)) => {
                            return Ok(futures::Async::Ready(Some(event)));
                        }
                        Ok(futures::Async::NotReady) => return Ok(futures::Async::NotReady),
                        Err(err) => {
                            if err.is_user_error() {
                                break Some(err);
                            } else {
                                log::warn!("client will reconnect because of error: {}", err);

                                if !err.session_is_resumable() {
                                    // Ensure clean session if the error is such that the session is not resumable.
                                    //
                                    // DEVNOTE: subscriptions::State relies on the fact that the session is reset here.
                                    // Update that if this ever changes.
                                    *client_id = match std::mem::replace(
                                        client_id,
                                        crate::proto::ClientId::ServerGenerated,
                                    ) {
                                        id @ crate::proto::ClientId::ServerGenerated
                                        | id @ crate::proto::ClientId::IdWithCleanSession(_) => id,
                                        crate::proto::ClientId::IdWithExistingSession(id) => {
                                            crate::proto::ClientId::IdWithCleanSession(id)
                                        }
                                    };
                                }

                                connect.reconnect();
                            }
                        }
                    }
                }

                ClientState::ShuttingDown {
                    client_id,
                    username,
                    password,
                    will,
                    keep_alive,

                    connect,

                    sent_disconnect,

                    reason,
                } => {
                    let self::connect::Connected { framed, .. } = match connect.poll(
                        username.as_ref().map(AsRef::as_ref),
                        password.as_ref().map(AsRef::as_ref),
                        will.as_ref(),
                        client_id,
                        *keep_alive,
                    ) {
                        Ok(futures::Async::Ready(framed)) => framed,
                        Ok(futures::Async::NotReady) => {
                            // Already disconnected
                            self.0 = ClientState::ShutDown {
                                reason: reason.take(),
                            };
                            continue;
                        }
                        Err(()) => unreachable!(),
                    };

                    loop {
                        if *sent_disconnect {
                            match framed.poll_complete().map_err(Error::EncodePacket) {
                                Ok(futures::Async::Ready(())) => {
                                    self.0 = ClientState::ShutDown {
                                        reason: reason.take(),
                                    };
                                    break;
                                }

                                Ok(futures::Async::NotReady) => return Ok(futures::Async::NotReady),

                                Err(err) => {
                                    log::warn!("couldn't send DISCONNECT: {}", err);
                                    self.0 = ClientState::ShutDown {
                                        reason: reason.take(),
                                    };
                                    break;
                                }
                            }
                        } else {
                            match framed.start_send(crate::proto::Packet::Disconnect) {
                                Ok(futures::AsyncSink::Ready) => *sent_disconnect = true,

                                Ok(futures::AsyncSink::NotReady(_)) => {
                                    return Ok(futures::Async::NotReady);
                                }

                                Err(err) => {
                                    log::warn!("couldn't send DISCONNECT: {}", err);
                                    self.0 = ClientState::ShutDown {
                                        reason: reason.take(),
                                    };
                                    break;
                                }
                            }
                        }
                    }
                }

                ClientState::ShutDown { reason } => match reason.take() {
                    Some(err) => return Err(err),
                    None => return Ok(futures::Async::Ready(None)),
                },
            }
        };

        // If we're here, then we're transitioning from Up to ShuttingDown

        match std::mem::replace(&mut self.0, ClientState::ShutDown { reason: None }) {
            ClientState::Up {
                client_id,
                username,
                password,
                will,
                keep_alive,

                connect,
                ..
            } => {
                log::warn!("Shutting down...");

                self.0 = ClientState::ShuttingDown {
                    client_id,
                    username,
                    password,
                    will,
                    keep_alive,

                    connect,

                    sent_disconnect: false,

                    reason,
                };
                self.poll()
            }

            _ => unreachable!(),
        }
    }
}

/// This trait provides an I/O object that a [`Client`] can use.
///
/// The trait is automatically implemented for all [`FnMut`] that return a connection future.
pub trait IoSource {
    /// The I/O object
    type Io: tokio::io::AsyncRead + tokio::io::AsyncWrite;

    /// The connection future
    type Future: Future<Item = Self::Io>;

    /// Attempts the connection and returns a [`Future`] that resolves when the connection succeeds
    fn connect(&mut self) -> Self::Future;
}

impl<F, A> IoSource for F
where
    F: FnMut() -> A,
    A: Future,
    <A as Future>::Item: tokio::io::AsyncRead + tokio::io::AsyncWrite,
{
    type Io = <A as Future>::Item;
    type Future = A;

    fn connect(&mut self) -> Self::Future {
        (self)()
    }
}

/// An event generated by the [`Client`]
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// The [`Client`] established a new connection to the server.
    NewConnection {
        /// Whether the session was reset as part of this new connection or not
        reset_session: bool,
    },

    /// A publication received from the server
    Publication(ReceivedPublication),

    /// Subscription updates acked by the server
    SubscriptionUpdates(Vec<crate::SubscriptionUpdate>),
}

/// A message that was received from the server
#[derive(Debug, PartialEq, Eq)]
pub struct ReceivedPublication {
    pub topic_name: String,
    pub dup: bool,
    pub qos: crate::proto::QoS,
    pub retain: bool,
    pub payload: Vec<u8>,
}

pub struct ShutdownHandle(futures::sync::mpsc::Sender<()>);

impl ShutdownHandle {
    /// Signals the [`Client`] to shut down.
    ///
    /// The returned `Future` resolves when the `Client` is guaranteed the notification,
    /// not necessarily when the `Client` has completed shutting down.
    pub fn shutdown(&self) -> impl Future<Item = (), Error = ShutdownError> {
        self.0.clone().send(()).then(|result| match result {
            Ok(_) => Ok(()),
            Err(_) => Err(ShutdownError::ClientDoesNotExist),
        })
    }
}

#[derive(Debug)]
enum ClientState<IoS>
where
    IoS: IoSource,
{
    Up {
        client_id: crate::proto::ClientId,
        username: Option<String>,
        password: Option<String>,
        will: Option<crate::proto::Publication>,
        keep_alive: std::time::Duration,

        shutdown_send: futures::sync::mpsc::Sender<()>,
        shutdown_recv: futures::sync::mpsc::Receiver<()>,

        packet_identifiers: PacketIdentifiers,

        connect: self::connect::Connect<IoS>,
        ping: self::ping::State,
        publish: self::publish::State,
        subscriptions: self::subscriptions::State,

        /// Packets waiting to be written to the underlying `Framed`
        packets_waiting_to_be_sent: std::collections::VecDeque<crate::proto::Packet>,
    },

    ShuttingDown {
        client_id: crate::proto::ClientId,
        username: Option<String>,
        password: Option<String>,
        will: Option<crate::proto::Publication>,
        keep_alive: std::time::Duration,

        connect: self::connect::Connect<IoS>,

        /// If the DISCONNECT packet has already been sent
        sent_disconnect: bool,

        /// The Error that caused the Client to transition away from Up, if any
        reason: Option<Error>,
    },

    ShutDown {
        /// The Error that caused the Client to transition away from Up, if any
        reason: Option<Error>,
    },
}

fn client_poll<S>(
    framed: &mut crate::logging_framed::LoggingFramed<S>,
    keep_alive: std::time::Duration,
    packets_waiting_to_be_sent: &mut std::collections::VecDeque<crate::proto::Packet>,
    packet_identifiers: &mut PacketIdentifiers,
    ping: &mut self::ping::State,
    publish: &mut self::publish::State,
    subscriptions: &mut self::subscriptions::State,
) -> futures::Poll<Event, Error>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite,
{
    loop {
        // Begin sending any packets waiting to be sent
        while let Some(packet) = packets_waiting_to_be_sent.pop_front() {
            match framed.start_send(packet).map_err(Error::EncodePacket)? {
                futures::AsyncSink::Ready => (),

                futures::AsyncSink::NotReady(packet) => {
                    packets_waiting_to_be_sent.push_front(packet);
                    break;
                }
            }
        }

        // Finish sending any packets waiting to be sent.
        //
        // We don't care whether this returns Async::NotReady or Ready.
        let _ = framed.poll_complete().map_err(Error::EncodePacket)?;

        let mut continue_loop = false;

        let mut packet = match framed.poll().map_err(Error::DecodePacket)? {
            futures::Async::Ready(Some(packet)) => {
                // May have more packets after this one, so keep looping
                continue_loop = true;
                Some(packet)
            }
            futures::Async::Ready(None) => return Err(Error::ServerClosedConnection),
            futures::Async::NotReady => None,
        };

        let mut new_packets_to_be_sent = vec![];

        // Ping
        match ping.poll(&mut packet, keep_alive)? {
            futures::Async::Ready(packet) => new_packets_to_be_sent.push(packet),
            futures::Async::NotReady => (),
        }

        // Publish
        let (new_publish_packets, publication_received) =
            publish.poll(&mut packet, packet_identifiers)?;
        new_packets_to_be_sent.extend(new_publish_packets);

        // Subscriptions
        let subscription_updates = if publication_received.is_some() {
            vec![]
        } else {
            let (new_subscription_packets, subscription_updates) =
                subscriptions.poll(&mut packet, packet_identifiers)?;
            new_packets_to_be_sent.extend(new_subscription_packets);
            subscription_updates
        };

        assert!(packet.is_none(), "unconsumed packet");

        if !new_packets_to_be_sent.is_empty() {
            // Have new packets to send, so keep looping
            continue_loop = true;
            packets_waiting_to_be_sent.extend(new_packets_to_be_sent);
        }

        if let Some(publication_received) = publication_received {
            return Ok(futures::Async::Ready(Event::Publication(
                publication_received,
            )));
        }

        if !subscription_updates.is_empty() {
            return Ok(futures::Async::Ready(Event::SubscriptionUpdates(
                subscription_updates,
            )));
        }

        if !continue_loop {
            return Ok(futures::Async::NotReady);
        }
    }
}

struct PacketIdentifiers {
    in_use: Box<[usize; PacketIdentifiers::SIZE]>,
    previous: crate::proto::PacketIdentifier,
}

impl PacketIdentifiers {
    /// Size of a bitset for every packet identifier
    ///
    /// Packet identifiers are u16's, so the number of usize's required
    /// = number of u16's / number of bits in a usize
    /// = pow(2, number of bits in a u16) / number of bits in a usize
    /// = pow(2, 16) / (size_of::<usize>() * 8)
    ///
    /// We use a bitshift instead of usize::pow because the latter is not a const fn
    const SIZE: usize = (1 << 16) / (std::mem::size_of::<usize>() * 8);

    fn reserve(&mut self) -> Result<crate::proto::PacketIdentifier, Error> {
        let start = self.previous;
        let mut current = start;

        current += 1;

        let (block, mask) = self.entry(current);
        if (*block & mask) != 0 {
            return Err(Error::PacketIdentifiersExhausted);
        }

        *block |= mask;
        self.previous = current;
        Ok(current)
    }

    fn discard(&mut self, packet_identifier: crate::proto::PacketIdentifier) {
        let (block, mask) = self.entry(packet_identifier);
        *block &= !mask;
    }

    fn entry(&mut self, packet_identifier: crate::proto::PacketIdentifier) -> (&mut usize, usize) {
        let packet_identifier = usize::from(packet_identifier.get());
        let (block, offset) = (
            packet_identifier / (std::mem::size_of::<usize>() * 8),
            packet_identifier % (std::mem::size_of::<usize>() * 8),
        );
        (&mut self.in_use[block], 1 << offset)
    }
}

impl std::fmt::Debug for PacketIdentifiers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PacketIdentifiers")
            .field("previous", &self.previous)
            .finish()
    }
}

impl Default for PacketIdentifiers {
    fn default() -> Self {
        PacketIdentifiers {
            in_use: Box::new([0; PacketIdentifiers::SIZE]),
            previous: crate::proto::PacketIdentifier::max_value(),
        }
    }
}

#[derive(Debug)]
pub enum Error {
    DecodePacket(crate::proto::DecodeError),
    DuplicateExactlyOncePublishPacketNotMarkedDuplicate(crate::proto::PacketIdentifier),
    EncodePacket(crate::proto::EncodeError),
    PacketIdentifiersExhausted,
    PingTimer(tokio::timer::Error),
    ServerClosedConnection,
    SubAckDoesNotContainEnoughQoS(crate::proto::PacketIdentifier, usize, usize),
    SubscriptionDowngraded(String, crate::proto::QoS, crate::proto::QoS),
    SubscriptionRejectedByServer,
    UnexpectedSubAck(crate::proto::PacketIdentifier, UnexpectedSubUnsubAckReason),
    UnexpectedUnsubAck(crate::proto::PacketIdentifier, UnexpectedSubUnsubAckReason),
}

#[derive(Clone, Copy, Debug)]
pub enum UnexpectedSubUnsubAckReason {
    DidNotExpect,
    Expected(crate::proto::PacketIdentifier),
    ExpectedSubAck(crate::proto::PacketIdentifier),
    ExpectedUnsubAck(crate::proto::PacketIdentifier),
}

impl Error {
    fn is_user_error(&self) -> bool {
        match self {
            Error::EncodePacket(err) => err.is_user_error(),
            _ => false,
        }
    }

    fn session_is_resumable(&self) -> bool {
        match self {
            Error::DecodePacket(crate::proto::DecodeError::Io(err)) => {
                err.kind() == std::io::ErrorKind::TimedOut
            }
            Error::ServerClosedConnection => true,
            _ => false,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
			Error::DecodePacket(err) =>
				write!(f, "could not decode packet: {}", err),

			Error::DuplicateExactlyOncePublishPacketNotMarkedDuplicate(packet_identifier) =>
				write!(
					f,
					"server sent a new ExactlyOnce PUBLISH packet {} with the same packet identifier as another unacknowledged ExactlyOnce PUBLISH packet",
					packet_identifier,
				),

			Error::EncodePacket(err) =>
				write!(f, "could not encode packet: {}", err),

			Error::PacketIdentifiersExhausted =>
				write!(f, "all packet identifiers exhausted"),

			Error::PingTimer(err) =>
				write!(f, "ping timer failed: {}", err),

			Error::ServerClosedConnection =>
				write!(f, "connection closed by server"),

			Error::SubAckDoesNotContainEnoughQoS(packet_identifier, expected, actual) =>
				write!(f, "Expected SUBACK {} to contain {} QoS's but it actually contained {}", packet_identifier, expected, actual),

			Error::SubscriptionDowngraded(topic_name, expected, actual) =>
				write!(f, "Server downgraded subscription for topic filter {:?} with QoS {:?} to {:?}", topic_name, expected, actual),

			Error::SubscriptionRejectedByServer =>
				write!(f, "Server rejected one or more subscriptions"),

			Error::UnexpectedSubAck(packet_identifier, reason) =>
				write!(f, "received SUBACK {} but {}", packet_identifier, reason),

			Error::UnexpectedUnsubAck(packet_identifier, reason) =>
				write!(f, "received UNSUBACK {} but {}", packet_identifier, reason),
		}
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        #[allow(clippy::match_same_arms)]
        match self {
            Error::DecodePacket(err) => Some(err),
            Error::DuplicateExactlyOncePublishPacketNotMarkedDuplicate(_) => None,
            Error::EncodePacket(err) => Some(err),
            Error::PacketIdentifiersExhausted => None,
            Error::PingTimer(err) => Some(err),
            Error::ServerClosedConnection => None,
            Error::SubAckDoesNotContainEnoughQoS(_, _, _) => None,
            Error::SubscriptionDowngraded(_, _, _) => None,
            Error::SubscriptionRejectedByServer => None,
            Error::UnexpectedSubAck(_, _) => None,
            Error::UnexpectedUnsubAck(_, _) => None,
        }
    }
}

impl std::fmt::Display for UnexpectedSubUnsubAckReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnexpectedSubUnsubAckReason::DidNotExpect => write!(f, "did not expect it"),
            UnexpectedSubUnsubAckReason::Expected(packet_identifier) => {
                write!(f, "expected {}", packet_identifier)
            }
            UnexpectedSubUnsubAckReason::ExpectedSubAck(packet_identifier) => {
                write!(f, "expected SUBACK {}", packet_identifier)
            }
            UnexpectedSubUnsubAckReason::ExpectedUnsubAck(packet_identifier) => {
                write!(f, "expected UNSUBACK {}", packet_identifier)
            }
        }
    }
}

#[derive(Debug)]
pub enum ShutdownError {
    ClientDoesNotExist,
}

impl std::fmt::Display for ShutdownError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShutdownError::ClientDoesNotExist => write!(f, "client does not exist"),
        }
    }
}

impl std::error::Error for ShutdownError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_identifiers() {
        #[cfg(target_pointer_width = "32")]
        assert_eq!(PacketIdentifiers::SIZE, 2048);
        #[cfg(target_pointer_width = "64")]
        assert_eq!(PacketIdentifiers::SIZE, 1024);

        let mut packet_identifiers: PacketIdentifiers = Default::default();
        assert_eq!(
            packet_identifiers.in_use[..],
            Box::new([0; PacketIdentifiers::SIZE])[..]
        );

        assert_eq!(packet_identifiers.reserve().unwrap().get(), 1);
        let mut expected = Box::new([0; PacketIdentifiers::SIZE]);
        expected[0] = 1 << 1;
        assert_eq!(packet_identifiers.in_use[..], expected[..]);

        assert_eq!(packet_identifiers.reserve().unwrap().get(), 2);
        let mut expected = Box::new([0; PacketIdentifiers::SIZE]);
        expected[0] = (1 << 1) | (1 << 2);
        assert_eq!(packet_identifiers.in_use[..], expected[..]);

        assert_eq!(packet_identifiers.reserve().unwrap().get(), 3);
        let mut expected = Box::new([0; PacketIdentifiers::SIZE]);
        expected[0] = (1 << 1) | (1 << 2) | (1 << 3);
        assert_eq!(packet_identifiers.in_use[..], expected[..]);

        packet_identifiers.discard(crate::proto::PacketIdentifier::new(2).unwrap());
        let mut expected = Box::new([0; PacketIdentifiers::SIZE]);
        expected[0] = (1 << 1) | (1 << 3);
        assert_eq!(packet_identifiers.in_use[..], expected[..]);

        assert_eq!(packet_identifiers.reserve().unwrap().get(), 4);
        let mut expected = Box::new([0; PacketIdentifiers::SIZE]);
        expected[0] = (1 << 1) | (1 << 3) | (1 << 4);
        assert_eq!(packet_identifiers.in_use[..], expected[..]);

        packet_identifiers.discard(crate::proto::PacketIdentifier::new(1).unwrap());
        let mut expected = Box::new([0; PacketIdentifiers::SIZE]);
        expected[0] = (1 << 3) | (1 << 4);
        assert_eq!(packet_identifiers.in_use[..], expected[..]);

        packet_identifiers.discard(crate::proto::PacketIdentifier::new(3).unwrap());
        let mut expected = Box::new([0; PacketIdentifiers::SIZE]);
        expected[0] = 1 << 4;
        assert_eq!(packet_identifiers.in_use[..], expected[..]);

        packet_identifiers.discard(crate::proto::PacketIdentifier::new(4).unwrap());
        assert_eq!(
            packet_identifiers.in_use[..],
            Box::new([0; PacketIdentifiers::SIZE])[..]
        );

        assert_eq!(packet_identifiers.reserve().unwrap().get(), 5);
        let mut expected = Box::new([0; PacketIdentifiers::SIZE]);
        expected[0] = 1 << 5;
        assert_eq!(packet_identifiers.in_use[..], expected[..]);

        let goes_in_next_block = std::mem::size_of::<usize>() * 8;
        #[allow(clippy::cast_possible_truncation)]
        for i in 6..=goes_in_next_block {
            assert_eq!(packet_identifiers.reserve().unwrap().get(), i as u16);
        }
        let mut expected = Box::new([0; PacketIdentifiers::SIZE]);
        #[allow(clippy::identity_op)]
        {
            expected[0] = usize::max_value() - (1 << 0) - (1 << 1) - (1 << 2) - (1 << 3) - (1 << 4);
            expected[1] |= 1 << 0;
        }
        assert_eq!(packet_identifiers.in_use[..], expected[..]);

        #[allow(clippy::cast_possible_truncation, clippy::range_minus_one)]
        for i in 5..=(goes_in_next_block - 1) {
            packet_identifiers.discard(crate::proto::PacketIdentifier::new(i as u16).unwrap());
        }
        let mut expected = Box::new([0; PacketIdentifiers::SIZE]);
        #[allow(clippy::identity_op)]
        {
            expected[1] |= 1 << 0;
        }
        assert_eq!(packet_identifiers.in_use[..], expected[..]);
    }
}
