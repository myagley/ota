use futures::{Future, Stream};

pub(crate) fn verify_client_events(
    runtime: &mut tokio::runtime::current_thread::Runtime,
    client: mqtt::Client<IoSource>,
    expected: Vec<mqtt::Event>,
) {
    let mut expected = expected.into_iter();

    runtime.spawn(
        client
            .map_err(|err| panic!("{:?}", err))
            .for_each(move |event| {
                assert_eq!(expected.next(), Some(event));
                Ok(())
            }),
    );
}

/// An `mqtt::IoSource` impl suitable for use with an `mqtt::Client`. The IoSource pretends to provide connections
/// to a real MQTT server.
#[derive(Debug)]
pub(crate) struct IoSource(std::vec::IntoIter<TestConnection>);

impl IoSource {
    /// Each element of `server_steps` represents a single connection between the client and server. The element contains
    /// an ordered sequence of what packets the server expects to send or receive in that connection.
    ///
    /// When the connection is broken by the server, the current element is dropped. When the client reconnects,
    /// it is served based on the next element.
    ///
    /// The second value returned by this function is a future that resolves when all connections have been dropped,
    /// *and* each connection's packets were used up completely before that connection was dropped.
    /// If any connection is dropped before its packets have been used up, the future will resolve to an error.
    pub(crate) fn new(
        server_steps: Vec<Vec<TestConnectionStep<mqtt::proto::Packet, mqtt::proto::Packet>>>,
    ) -> (
        Self,
        impl Future<Item = (), Error = futures::sync::oneshot::Canceled>,
    ) {
        use tokio::codec::Encoder;

        let mut connections = Vec::with_capacity(server_steps.len());
        let mut done: Box<Future<Item = _, Error = _> + Send> = Box::new(futures::future::ok(()));

        for server_steps in server_steps {
            let steps = server_steps
                .into_iter()
                .map(|step| match step {
                    TestConnectionStep::Receives(packet) => {
                        TestConnectionStep::Receives((packet, bytes::BytesMut::new()))
                    }

                    TestConnectionStep::Sends(packet) => {
                        let mut packet_codec: mqtt::proto::PacketCodec = Default::default();
                        let mut bytes = bytes::BytesMut::new();
                        packet_codec.encode(packet.clone(), &mut bytes).unwrap();
                        TestConnectionStep::Sends((packet, std::io::Cursor::new(bytes)))
                    }
                })
                .collect();

            let (done_send, done_recv) = futures::sync::oneshot::channel();

            connections.push(TestConnection {
                steps,
                done_send: Some(done_send),
            });

            done = Box::new(done.join(done_recv).map(|(_, ())| ()));
        }

        (IoSource(connections.into_iter()), done)
    }
}

impl mqtt::IoSource for IoSource {
    type Io = TestConnection;
    type Future = Box<Future<Item = Self::Io, Error = std::io::Error> + Send>;

    fn connect(&mut self) -> Self::Future {
        println!("client is creating new connection");

        if let Some(io) = self.0.next() {
            Box::new(futures::future::ok(io))
        } else {
            // The client drops the previous Io (TestConnection) before requesting a new one from the IoSource.
            // Dropping the TestConnection would have dropped the futures::sync::oneshot::Sender inside it.
            //
            // If the client is requesting a new Io because the last TestConnection ran out of steps and broke the previous connection,
            // then that TestConnection would've already used its sender to signal the future held by the test.
            // We can just delay a bit here till the test receives the signal and exits.
            //
            // If the connection broke while there were still steps remaining in the TestConnection, then the dropped sender will cause the test
            // to receive a futures::sync::oneshot::Canceled error, so the test will panic before this deadline elapses anyway.
            Box::new(
                tokio::timer::Delay::new(
                    std::time::Instant::now() + std::time::Duration::from_secs(5),
                )
                .then(|result| -> std::io::Result<_> {
                    let _ = result.unwrap();
                    unreachable!();
                }),
            )
        }
    }
}

/// A single connection between a client and a server
#[derive(Debug)]
pub(crate) struct TestConnection {
    steps: std::collections::VecDeque<
        TestConnectionStep<
            (mqtt::proto::Packet, bytes::BytesMut),
            (mqtt::proto::Packet, std::io::Cursor<bytes::BytesMut>),
        >,
    >,
    done_send: Option<futures::sync::oneshot::Sender<()>>,
}

/// A single step in the connection between a client and a server
#[derive(Debug)]
pub(crate) enum TestConnectionStep<TReceives, TSends> {
    Receives(TReceives),
    Sends(TSends),
}

impl std::io::Read for TestConnection {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let (read, step_done) = match self.steps.front_mut() {
            Some(TestConnectionStep::Receives(_)) => {
                println!(
                    "client is reading from server but server wants to receive something first"
                );

                // Since the TestConnection always makes progress with either Read or Write, we don't need to register for wakeup here.
                return Err(std::io::ErrorKind::WouldBlock.into());
            }

            Some(TestConnectionStep::Sends((packet, cursor))) => {
                println!("server sends {:?}", packet);
                let read = cursor.read(buf)?;
                (read, cursor.position() == cursor.get_ref().len() as u64)
            }

            None => {
                if let Some(done_send) = self.done_send.take() {
                    done_send.send(()).unwrap();
                }

                (0, false)
            }
        };

        if step_done {
            let _ = self.steps.pop_front();
        }

        println!("client read {} bytes from server", read);

        Ok(read)
    }
}

impl tokio::io::AsyncRead for TestConnection {}

impl std::io::Write for TestConnection {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        use tokio::codec::Decoder;

        let (written, step_done) = match self.steps.front_mut() {
            Some(TestConnectionStep::Receives((expected_packet, bytes))) => {
                println!("server expects to receive {:?}", expected_packet);

                let previous_bytes_len = bytes.len();

                bytes.extend_from_slice(buf);

                let mut packet_codec: mqtt::proto::PacketCodec = Default::default();
                match packet_codec.decode(bytes) {
                    Ok(Some(actual_packet)) => {
                        // Codec will remove the bytes it's parsed successfully, so whatever's left is what didn't get parsed
                        let written = previous_bytes_len + buf.len() - bytes.len();

                        println!("server received {:?}", actual_packet);
                        assert_eq!(*expected_packet, actual_packet);

                        (written, true)
                    }

                    Ok(None) => (buf.len(), false),

                    Err(err) => panic!("{:?}", err),
                }
            }

            Some(TestConnectionStep::Sends(_)) => {
                println!("client is writing to server but server wants to send something first");

                // Since the TestConnection always makes progress with either Read or Write, we don't need to register for wakeup here.
                return Err(std::io::ErrorKind::WouldBlock.into());
            }

            None => {
                if let Some(done_send) = self.done_send.take() {
                    done_send.send(()).unwrap();
                }

                (0, false)
            }
        };

        if step_done {
            let _ = self.steps.pop_front();
        }

        println!("client wrote {} bytes to server", written);

        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl tokio::io::AsyncWrite for TestConnection {
    fn shutdown(&mut self) -> futures::Poll<(), std::io::Error> {
        Ok(futures::Async::Ready(()))
    }
}
