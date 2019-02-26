use futures::{Future, Sink, Stream};

#[derive(Debug)]
pub(super) struct Connect<IoS>
where
    IoS: super::IoSource,
{
    io_source: IoS,
    max_back_off: std::time::Duration,
    current_back_off: std::time::Duration,
    state: State<IoS>,
}

enum State<IoS>
where
    IoS: super::IoSource,
{
    BeginBackOff,
    EndBackOff(tokio::timer::Delay),
    BeginConnecting,
    WaitingForIoToConnect(<IoS as super::IoSource>::Future),
    Framed(
        crate::logging_framed::LoggingFramed<<IoS as super::IoSource>::Io>,
        FramedState,
    ),
}

#[derive(Clone, Copy, Debug)]
enum FramedState {
    BeginSendingConnect,
    EndSendingConnect,
    WaitingForConnAck,
    Connected {
        new_connection: bool,
        reset_session: bool,
    },
}

impl<IoS> std::fmt::Debug for State<IoS>
where
    IoS: super::IoSource,
{
    #[allow(
		clippy::unneeded_field_pattern, // Clippy wants wildcard pattern for Connected,
		                                // which would silently allow fields to be added to the variant without adding them here
	)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            State::BeginBackOff => f.write_str("BeginBackOff"),
            State::EndBackOff(_) => f.write_str("EndBackOff"),
            State::BeginConnecting => f.write_str("BeginConnecting"),
            State::WaitingForIoToConnect(_) => f.write_str("WaitingForIoToConnect"),
            State::Framed(_, framed_state) => f.debug_tuple("Framed").field(framed_state).finish(),
        }
    }
}

impl<IoS> Connect<IoS>
where
    IoS: super::IoSource,
{
    pub(super) fn new(io_source: IoS, max_back_off: std::time::Duration) -> Self {
        Connect {
            io_source,
            max_back_off,
            current_back_off: std::time::Duration::from_secs(0),
            state: State::BeginConnecting,
        }
    }

    pub(super) fn reconnect(&mut self) {
        self.state = State::BeginBackOff;
    }
}

impl<IoS> Connect<IoS>
where
    IoS: super::IoSource,
    <<IoS as super::IoSource>::Future as Future>::Error: std::fmt::Display,
{
    pub(super) fn poll<'a>(
        &'a mut self,
        username: Option<&str>,
        password: Option<&str>,
        will: Option<&crate::proto::Publication>,
        client_id: &mut crate::proto::ClientId,
        keep_alive: std::time::Duration,
    ) -> futures::Poll<Connected<'a, IoS>, ()> {
        let state = &mut self.state;

        loop {
            log::trace!("    {:?}", state);

            match state {
                State::BeginBackOff => match self.current_back_off {
                    back_off if back_off.as_secs() == 0 => {
                        self.current_back_off = std::time::Duration::from_secs(1);
                        *state = State::BeginConnecting;
                    }

                    back_off => {
                        log::debug!("Backing off for {:?}", back_off);
                        let back_off_deadline = std::time::Instant::now() + back_off;
                        self.current_back_off =
                            std::cmp::min(self.max_back_off, self.current_back_off * 2);
                        *state = State::EndBackOff(tokio::timer::Delay::new(back_off_deadline));
                    }
                },

                State::EndBackOff(back_off_timer) => match back_off_timer
                    .poll()
                    .expect("could not poll back-off timer")
                {
                    futures::Async::Ready(()) => *state = State::BeginConnecting,
                    futures::Async::NotReady => return Ok(futures::Async::NotReady),
                },

                State::BeginConnecting => {
                    let io = self.io_source.connect();
                    *state = State::WaitingForIoToConnect(io);
                }

                State::WaitingForIoToConnect(io) => match io.poll() {
                    Ok(futures::Async::Ready(io)) => {
                        let framed = crate::logging_framed::LoggingFramed::new(io);
                        *state = State::Framed(framed, FramedState::BeginSendingConnect);
                    }

                    Ok(futures::Async::NotReady) => return Ok(futures::Async::NotReady),

                    Err(err) => {
                        log::warn!("could not connect to server: {}", err);
                        *state = State::BeginBackOff;
                    }
                },

                State::Framed(framed, framed_state @ FramedState::BeginSendingConnect) => {
                    let packet = crate::proto::Packet::Connect {
                        username: username.map(ToOwned::to_owned),
                        password: password.map(ToOwned::to_owned),
                        will: will.cloned(),
                        client_id: client_id.clone(),
                        keep_alive,
                    };

                    match framed.start_send(packet) {
                        Ok(futures::AsyncSink::Ready) => {
                            *framed_state = FramedState::EndSendingConnect
                        }
                        Ok(futures::AsyncSink::NotReady(_)) => return Ok(futures::Async::NotReady),
                        Err(err) => {
                            log::warn!("could not connect to server: {}", err);
                            *state = State::BeginBackOff;
                        }
                    }
                }

                State::Framed(framed, framed_state @ FramedState::EndSendingConnect) => {
                    match framed.poll_complete() {
                        Ok(futures::Async::Ready(())) => {
                            *framed_state = FramedState::WaitingForConnAck
                        }
                        Ok(futures::Async::NotReady) => return Ok(futures::Async::NotReady),
                        Err(err) => {
                            log::warn!("could not connect to server: {}", err);
                            *state = State::BeginBackOff;
                        }
                    }
                }

                State::Framed(framed, framed_state @ FramedState::WaitingForConnAck) => {
                    match framed.poll() {
                        Ok(futures::Async::Ready(Some(packet))) => match packet {
                            crate::proto::Packet::ConnAck {
                                session_present,
                                return_code: crate::proto::ConnectReturnCode::Accepted,
                            } => {
                                self.current_back_off = std::time::Duration::from_secs(0);

                                let reset_session = match client_id {
                                    crate::proto::ClientId::ServerGenerated => true,
                                    crate::proto::ClientId::IdWithCleanSession(id) => {
                                        *client_id = crate::proto::ClientId::IdWithExistingSession(
                                            std::mem::replace(id, Default::default()),
                                        );
                                        true
                                    }
                                    crate::proto::ClientId::IdWithExistingSession(id) => {
                                        *client_id = crate::proto::ClientId::IdWithExistingSession(
                                            std::mem::replace(id, Default::default()),
                                        );
                                        !session_present
                                    }
                                };

                                *framed_state = FramedState::Connected {
                                    new_connection: true,
                                    reset_session,
                                };
                            }

                            crate::proto::Packet::ConnAck {
                                return_code: crate::proto::ConnectReturnCode::Refused(return_code),
                                ..
                            } => {
                                log::warn!(
                                    "could not connect to server: connection refused: {:?}",
                                    return_code
                                );
                                *state = State::BeginBackOff;
                            }

                            packet => {
                                log::warn!("could not connect to server: expected to receive ConnAck but received {:?}", packet);
                                *state = State::BeginBackOff;
                            }
                        },

                        Ok(futures::Async::Ready(None)) => {
                            log::warn!("could not connect to server: connection closed by server");
                            *state = State::BeginBackOff;
                        }

                        Ok(futures::Async::NotReady) => return Ok(futures::Async::NotReady),

                        Err(err) => {
                            log::warn!("could not connect to server: {}", err);
                            *state = State::BeginBackOff;
                        }
                    }
                }

                State::Framed(
                    framed,
                    FramedState::Connected {
                        new_connection,
                        reset_session,
                    },
                ) => {
                    let result = Connected {
                        framed,
                        new_connection: *new_connection,
                        reset_session: *reset_session,
                    };
                    *new_connection = false;
                    *reset_session = false;
                    return Ok(futures::Async::Ready(result));
                }
            }
        }
    }
}

pub(super) struct Connected<'a, IoS>
where
    IoS: super::IoSource,
{
    pub(super) framed: &'a mut crate::logging_framed::LoggingFramed<<IoS as super::IoSource>::Io>,
    pub(super) new_connection: bool,
    pub(super) reset_session: bool,
}
