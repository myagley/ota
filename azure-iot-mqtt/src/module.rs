//! This module contains the module client and module message types.

use futures::Stream;

/// A client for the Azure IoT Hub MQTT protocol. This client receives module-level messages.
///
/// A `Client` is a [`Stream`] of [`Message`]s. These messages contain twin state messages and direct method requests.
///
/// It automatically reconnects if the connection to the server is broken. Each reconnection will yield one [`Message::TwinInitial`] message.
pub struct Client {
    inner: mqtt::Client<crate::IoSource>,

    state: State,
    previous_request_id: u8,

    desired_properties: crate::twin_state::desired::State,
    reported_properties: crate::twin_state::reported::State,

    direct_method_response_send: futures::sync::mpsc::Sender<crate::DirectMethodResponse>,
    direct_method_response_recv: futures::sync::mpsc::Receiver<crate::DirectMethodResponse>,
}

#[derive(Debug)]
enum State {
    WaitingForSubscriptions { reset_session: bool },

    Idle,
}

impl Client {
    /// Creates a new `Client`
    ///
    /// * `iothub_hostname`
    ///
    ///     The hostname of the Azure IoT Hub. Eg "foo.azure-devices.net"
    ///
    /// * `device_id`
    ///
    ///     The ID of the device.
    ///
    /// * `module_id`
    ///
    ///     The ID of the module.
    ///
    /// * `authentication`
    ///
    ///     The method this client should use to authorize with the Azure IoT Hub.
    ///
    /// * `transport`
    ///
    ///     The transport to use for the connection to the Azure IoT Hub.
    ///
    /// * `will`
    ///
    ///     If set, this message will be published by the server if this client disconnects uncleanly.
    ///     Use the handle from `.inner().shutdown_handle()` to disconnect cleanly.
    ///
    /// * `max_back_off`
    ///
    ///     Every connection failure or server error will double the back-off period, to a maximum of this value.
    ///
    /// * `keep_alive`
    ///
    ///     The keep-alive time advertised to the server. The client will ping the server at half this interval.
    #[allow(clippy::new_ret_no_self)]
    pub fn new(
        iothub_hostname: String,
        device_id: &str,
        module_id: &str,
        authentication: crate::Authentication,
        transport: crate::Transport,

        will: Option<Vec<u8>>,

        max_back_off: std::time::Duration,
        keep_alive: std::time::Duration,
    ) -> Result<Self, crate::CreateClientError> {
        let inner = crate::client_new(
            iothub_hostname,
            device_id,
            Some(module_id),
            authentication,
            transport,
            will,
            max_back_off,
            keep_alive,
        )?;

        let (direct_method_response_send, direct_method_response_recv) =
            futures::sync::mpsc::channel(0);

        Ok(Client {
            inner,

            state: State::WaitingForSubscriptions {
                reset_session: true,
            },
            previous_request_id: u8::max_value(),

            desired_properties: crate::twin_state::desired::State::new(max_back_off, keep_alive),
            reported_properties: crate::twin_state::reported::State::new(max_back_off, keep_alive),

            direct_method_response_send,
            direct_method_response_recv,
        })
    }

    /// Gets a reference to the inner `mqtt::Client`
    pub fn inner(&self) -> &mqtt::Client<crate::IoSource> {
        &self.inner
    }

    /// Returns a handle that can be used to respond to direct methods
    pub fn direct_method_response_handle(&self) -> crate::DirectMethodResponseHandle {
        crate::DirectMethodResponseHandle(self.direct_method_response_send.clone())
    }

    /// Returns a handle that can be used to publish reported twin state to the Azure IoT Hub
    pub fn report_twin_state_handle(&self) -> crate::ReportTwinStateHandle {
        self.reported_properties.report_twin_state_handle()
    }
}

impl Stream for Client {
    type Item = Message;
    type Error = mqtt::Error;

    fn poll(&mut self) -> futures::Poll<Option<Self::Item>, Self::Error> {
        loop {
            log::trace!("    {:?}", self.state);

            while let futures::Async::Ready(Some(direct_method_response)) = self
                .direct_method_response_recv
                .poll()
                .expect("Receiver::poll cannot fail")
            {
                let crate::DirectMethodResponse {
                    request_id,
                    status,
                    payload,
                    ack_sender,
                } = direct_method_response;
                let payload = serde_json::to_vec(&payload)
                    .expect("cannot fail to serialize serde_json::Value");
                let publication = mqtt::proto::Publication {
                    topic_name: format!("$iothub/methods/res/{}/?$rid={}", status, request_id),
                    qos: mqtt::proto::QoS::AtLeastOnce,
                    retain: false,
                    payload,
                };

                if ack_sender
                    .send(Box::new(self.inner.publish(publication)))
                    .is_err()
                {
                    log::debug!("could not send ack for direct method response because ack receiver has been dropped");
                }
            }

            match &mut self.state {
                State::WaitingForSubscriptions { reset_session } => {
                    if *reset_session {
                        match self.inner.poll()? {
							futures::Async::Ready(Some(mqtt::Event::NewConnection { .. })) => (),

							futures::Async::Ready(Some(mqtt::Event::Publication(publication))) => match InternalMessage::parse(publication) {
								Ok(InternalMessage::DirectMethod { name, payload, request_id }) =>
									return Ok(futures::Async::Ready(Some(Message::DirectMethod { name, payload, request_id }))),

								Ok(message @ InternalMessage::TwinState(_)) =>
									log::debug!("Discarding message {:?} because we haven't finished subscribing yet", message),

								Err(err) =>
									log::warn!("Discarding message that could not be parsed: {}", err),
							},

							futures::Async::Ready(Some(mqtt::Event::SubscriptionUpdates(_))) => {
								log::debug!("subscriptions acked by server");
								self.state = State::Idle;
							},

							futures::Async::Ready(None) => return Ok(futures::Async::Ready(None)),

							futures::Async::NotReady => return Ok(futures::Async::NotReady),
						}
                    } else {
                        self.state = State::Idle;
                    }
                }

                State::Idle => {
                    let mut continue_loop = false;

                    let mut twin_state_message = match self.inner.poll()? {
                        futures::Async::Ready(Some(mqtt::Event::NewConnection {
                            reset_session,
                        })) => {
                            self.state = State::WaitingForSubscriptions { reset_session };
                            self.desired_properties.new_connection();
                            self.reported_properties.new_connection();
                            continue;
                        }

                        futures::Async::Ready(Some(mqtt::Event::Publication(publication))) => {
                            match InternalMessage::parse(publication) {
                                Ok(InternalMessage::DirectMethod {
                                    name,
                                    payload,
                                    request_id,
                                }) => {
                                    return Ok(futures::Async::Ready(Some(Message::DirectMethod {
                                        name,
                                        payload,
                                        request_id,
                                    })));
                                }

                                Ok(InternalMessage::TwinState(message)) => {
                                    // There may be more messages, so continue the loop
                                    continue_loop = true;

                                    Some(message)
                                }

                                Err(err) => {
                                    log::warn!(
                                        "Discarding message that could not be parsed: {}",
                                        err
                                    );
                                    continue;
                                }
                            }
                        }

                        // Don't expect any subscription updates at this point
                        futures::Async::Ready(Some(mqtt::Event::SubscriptionUpdates(_))) => {
                            unreachable!()
                        }

                        futures::Async::Ready(None) => return Ok(futures::Async::Ready(None)),

                        futures::Async::NotReady => None,
                    };

                    match self.desired_properties.poll(
                        &mut self.inner,
                        &mut twin_state_message,
                        &mut self.previous_request_id,
                    ) {
                        Ok(crate::twin_state::Response::Message(
                            crate::twin_state::desired::Message::Initial(twin_state),
                        )) => {
                            self.reported_properties
                                .set_initial_state(twin_state.reported.properties.clone());
                            return Ok(futures::Async::Ready(Some(Message::TwinInitial(
                                twin_state,
                            ))));
                        }

                        Ok(crate::twin_state::Response::Message(
                            crate::twin_state::desired::Message::Patch(properties),
                        )) => {
                            return Ok(futures::Async::Ready(Some(Message::TwinPatch(properties))));
                        }

                        Ok(crate::twin_state::Response::Continue) => continue_loop = true,

                        Ok(crate::twin_state::Response::NotReady) => (),

                        Err(err) => {
                            log::warn!("Discarding message that could not be parsed: {}", err)
                        }
                    }

                    match self.reported_properties.poll(
                        &mut self.inner,
                        &mut twin_state_message,
                        &mut self.previous_request_id,
                    ) {
                        Ok(crate::twin_state::Response::Message(message)) => match message {
                            crate::twin_state::reported::Message::Reported(version) => {
                                return Ok(futures::Async::Ready(Some(Message::ReportedTwinState(
                                    version,
                                ))));
                            }
                        },
                        Ok(crate::twin_state::Response::Continue) => continue_loop = true,
                        Ok(crate::twin_state::Response::NotReady) => (),
                        Err(err) => {
                            log::warn!("Discarding message that could not be parsed: {}", err)
                        }
                    }

                    if let Some(twin_state_message) = twin_state_message {
                        // This can happen if the Azure IoT Hub responded to a reported property request that we aren't waiting for
                        // because we have since sent a new one
                        log::debug!("unconsumed twin state message {:?}", twin_state_message);
                    }

                    if !continue_loop {
                        return Ok(futures::Async::NotReady);
                    }
                }
            }
        }
    }
}

/// A message generated by a [`Client`]
#[derive(Debug)]
pub enum Message {
    /// A direct method invocation
    DirectMethod {
        name: String,
        payload: serde_json::Value,
        request_id: String,
    },

    /// The server acknowledged a report of the twin state. Contains the version number of the updated section.
    ReportedTwinState(usize),

    /// The full twin state, as currently stored in the Azure IoT Hub.
    TwinInitial(crate::TwinState),

    /// A patch to the twin state that should be applied to the current state to get the new state.
    TwinPatch(crate::TwinProperties),
}

#[derive(Debug)]
enum MessageParseError {
    Json(serde_json::Error),
    UnrecognizedMessage(crate::twin_state::MessageParseError),
}

impl std::fmt::Display for MessageParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MessageParseError::Json(err) => {
                write!(f, "could not parse payload as valid JSON: {}", err)
            }
            MessageParseError::UnrecognizedMessage(err) => {
                write!(f, "message could not be recognized: {}", err)
            }
        }
    }
}

impl std::error::Error for MessageParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        #[allow(clippy::match_same_arms)]
        match self {
            MessageParseError::Json(err) => Some(err),
            MessageParseError::UnrecognizedMessage(err) => Some(err),
        }
    }
}

#[derive(Debug)]
enum InternalMessage {
    DirectMethod {
        name: String,
        payload: serde_json::Value,
        request_id: String,
    },

    TwinState(crate::twin_state::InternalTwinStateMessage),
}

impl InternalMessage {
    fn parse(publication: mqtt::ReceivedPublication) -> Result<Self, MessageParseError> {
        if let Some(captures) = crate::DIRECT_METHOD_REGEX.captures(&publication.topic_name) {
            let name = captures[1].to_string();
            let payload =
                serde_json::from_slice(&publication.payload).map_err(MessageParseError::Json)?;
            let request_id = captures[2].to_string();

            Ok(InternalMessage::DirectMethod {
                name,
                payload,
                request_id,
            })
        } else {
            match crate::twin_state::InternalTwinStateMessage::parse(publication) {
                Ok(message) => Ok(InternalMessage::TwinState(message)),
                Err(err) => Err(MessageParseError::UnrecognizedMessage(err)),
            }
        }
    }
}
