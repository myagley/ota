use futures::Future;

pub(super) enum State {
    BeginWaitingForNextPing,
    WaitingForNextPing(tokio::timer::Delay),
}

impl State {
    pub(super) fn poll(
        &mut self,
        packet: &mut Option<crate::proto::Packet>,
        keep_alive: std::time::Duration,
    ) -> futures::Poll<crate::proto::Packet, super::Error> {
        if let Some(crate::proto::Packet::PingResp) = packet {
            let _ = packet.take();

            match self {
                State::BeginWaitingForNextPing => (),
                State::WaitingForNextPing(ping_timer) => {
                    ping_timer.reset(deadline(std::time::Instant::now(), keep_alive))
                }
            }
        }

        loop {
            log::trace!("    {:?}", self);

            match self {
                State::BeginWaitingForNextPing => {
                    let ping_timer =
                        tokio::timer::Delay::new(deadline(std::time::Instant::now(), keep_alive));
                    *self = State::WaitingForNextPing(ping_timer);
                }

                State::WaitingForNextPing(ping_timer) => {
                    match ping_timer.poll().map_err(super::Error::PingTimer)? {
                        futures::Async::Ready(()) => {
                            ping_timer.reset(deadline(ping_timer.deadline(), keep_alive));
                            return Ok(futures::Async::Ready(crate::proto::Packet::PingReq));
                        }

                        futures::Async::NotReady => return Ok(futures::Async::NotReady),
                    }
                }
            }
        }
    }

    pub(super) fn new_connection(&mut self) {
        *self = State::BeginWaitingForNextPing;
    }
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            State::BeginWaitingForNextPing => f.write_str("BeginWaitingForNextPing"),
            State::WaitingForNextPing { .. } => f.write_str("WaitingForNextPing"),
        }
    }
}

fn deadline(now: std::time::Instant, keep_alive: std::time::Duration) -> std::time::Instant {
    now + keep_alive / 2
}
