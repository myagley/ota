An MQTT v3.1.1 Client implementation in Rust


# Features

- Supports the entire protocol, including all three QoS levels and wills.
- Transparently handles keep-alive pings.
- Transparently reconnects when connection is broken or protocol errors, with back-off.
- Handles subscription and ongoing QoS 1 and QoS 2 publish workflows across reconnections. You don't need to resubscribe or republish messages when the connection is re-established.
- Agnostic to the underlying transport, so it can run over TCP, TLS, WebSockets, etc.
- Standard futures 0.1 and tokio 0.1 interface. The client is just a `futures::Stream` of publications received from the server. The underlying transport just needs to implement `tokio::io::AsyncRead` and `tokio::io::AsyncWrite`.


# Documentation

The crate is not published to crates.io yet. Please generate docs locally with `cargo doc`.


# Examples

See the `examples/` directory for examples of a publisher and subscriber, and for how to set a will.

See <https://github.com/arsing/azure-iot-mqtt> for a library that implements [the Azure IoT Hub MQTT protocol](https://docs.microsoft.com/en-us/azure/iot-hub/iot-hub-mqtt-support) using this client.


# License

TBD. Will be resolved before publishing.
