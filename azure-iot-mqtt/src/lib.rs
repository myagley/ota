//! This crate contains types related to the Azure IoT MQTT server.

#![deny(rust_2018_idioms, warnings)]
#![deny(clippy::all, clippy::pedantic)]
#![allow(
    clippy::cyclomatic_complexity,
    clippy::default_trait_access,
    clippy::doc_markdown,
    clippy::large_enum_variant,
    clippy::pub_enum_variant_names,
    clippy::similar_names,
    clippy::single_match_else,
    clippy::stutter,
    clippy::too_many_arguments,
    clippy::use_self
)]

use futures::{Future, Sink};

pub mod device;

mod io;
pub use self::io::{Io, IoSource, Transport};

pub mod module;

mod system_properties;
pub use self::system_properties::{IotHubAck, SystemProperties};

mod twin_state;
pub use self::twin_state::{
    ReportTwinStateHandle, ReportTwinStateRequest, TwinProperties, TwinState,
};

/// The type of authentication the client should use to connect to the Azure IoT Hub
#[derive(Debug)]
pub enum Authentication {
    SasToken(String),

    Certificate { der: Vec<u8>, password: String },
}

/// Errors from creating a device or module client
#[derive(Debug)]
pub enum CreateClientError {
    ResolveIotHubHostname(Option<std::io::Error>),
    WebSocketUrl(url::ParseError),
}

impl std::fmt::Display for CreateClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CreateClientError::ResolveIotHubHostname(Some(err)) => {
                write!(f, "could not resolve Azure IoT Hub hostname: {}", err)
            }
            CreateClientError::ResolveIotHubHostname(None) => write!(
                f,
                "could not resolve Azure IoT Hub hostname: no addresses found"
            ),
            CreateClientError::WebSocketUrl(err) => write!(
                f,
                "could not construct a valid URL for the Azure IoT Hub: {}",
                err
            ),
        }
    }
}

impl std::error::Error for CreateClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CreateClientError::ResolveIotHubHostname(Some(err)) => Some(err),
            CreateClientError::ResolveIotHubHostname(None) => None,
            CreateClientError::WebSocketUrl(err) => Some(err),
        }
    }
}

/// Used to respond to direct methods
#[derive(Clone)]
pub struct DirectMethodResponseHandle(futures::sync::mpsc::Sender<DirectMethodResponse>);

impl DirectMethodResponseHandle {
    /// Send a direct method response with the given parameters
    pub fn respond(
        &self,
        request_id: String,
        status: crate::Status,
        payload: serde_json::Value,
    ) -> impl Future<Item = (), Error = DirectMethodResponseError> {
        let (ack_sender, ack_receiver) = futures::sync::oneshot::channel();
        let ack_receiver = ack_receiver.map_err(|_| DirectMethodResponseError::ClientDoesNotExist);

        self.0
            .clone()
            .send(DirectMethodResponse {
                request_id,
                status,
                payload,
                ack_sender,
            })
            .then(|result| match result {
                Ok(_) => Ok(()),
                Err(_) => Err(DirectMethodResponseError::ClientDoesNotExist),
            })
            .and_then(|()| ack_receiver)
            .and_then(|publish| publish.map_err(|_| DirectMethodResponseError::ClientDoesNotExist))
    }
}

#[derive(Debug)]
pub enum DirectMethodResponseError {
    ClientDoesNotExist,
}

impl std::fmt::Display for DirectMethodResponseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DirectMethodResponseError::ClientDoesNotExist => write!(f, "client does not exist"),
        }
    }
}

impl std::error::Error for DirectMethodResponseError {}

struct DirectMethodResponse {
    request_id: String,
    status: crate::Status,
    payload: serde_json::Value,
    ack_sender: futures::sync::oneshot::Sender<
        Box<dyn Future<Item = (), Error = mqtt::PublishError> + Send>,
    >,
}

/// Represents the status code used in initial twin responses and device method responses
#[derive(Clone, Copy, Debug)]
pub enum Status {
    /// 200
    Ok,

    /// 204
    NoContent,

    /// 400
    BadRequest,

    /// 429
    TooManyRequests,

    /// 5xx
    Error(u32),

    /// Other
    Other(u32),
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        #[allow(clippy::match_same_arms)]
        match self {
            Status::Ok => write!(f, "200"),
            Status::NoContent => write!(f, "204"),
            Status::BadRequest => write!(f, "400"),
            Status::TooManyRequests => write!(f, "429"),
            Status::Error(raw) => write!(f, "{}", raw),
            Status::Other(raw) => write!(f, "{}", raw),
        }
    }
}

impl std::str::FromStr for Status {
    type Err = std::num::ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.parse()? {
            200 => Status::Ok,
            204 => Status::NoContent,
            400 => Status::BadRequest,
            429 => Status::TooManyRequests,
            raw if raw >= 500 && raw < 600 => Status::Error(raw),
            raw => Status::Other(raw),
        })
    }
}

fn client_new(
    iothub_hostname: String,

    device_id: &str,
    module_id: Option<&str>,

    authentication: crate::Authentication,
    transport: crate::Transport,

    will: Option<Vec<u8>>,

    max_back_off: std::time::Duration,
    keep_alive: std::time::Duration,
) -> Result<mqtt::Client<crate::IoSource>, crate::CreateClientError> {
    let client_id = if let Some(module_id) = &module_id {
        format!("{}/{}", device_id, module_id)
    } else {
        device_id.to_string()
    };

    let username = if let Some(module_id) = &module_id {
        format!(
            "{}/{}/{}/?api-version=2018-06-30",
            iothub_hostname, device_id, module_id
        )
    } else {
        format!("{}/{}/?api-version=2018-06-30", iothub_hostname, device_id)
    };

    let (password, certificate) = match authentication {
        crate::Authentication::SasToken(sas_token) => (Some(sas_token), None),
        crate::Authentication::Certificate { der, password } => (None, Some((der, password))),
    };

    let will = match (will, module_id.as_ref()) {
        (Some(payload), Some(module_id)) => Some((
            format!(
                "devices/{}/modules/{}/messages/events/",
                device_id, module_id
            ),
            payload,
        )),
        (Some(payload), None) => Some((format!("devices/{}/messages/events/", device_id), payload)),
        (None, _) => None,
    };
    let will = will.map(|(topic_name, payload)| mqtt::proto::Publication {
        topic_name,
        qos: mqtt::proto::QoS::AtMostOnce,
        retain: false,
        payload,
    });

    let io_source = crate::IoSource::new(
        iothub_hostname.into(),
        certificate.into(),
        2 * keep_alive,
        transport,
    )?;

    let mut inner = mqtt::Client::new(
        Some(client_id),
        Some(username),
        password,
        will,
        io_source,
        max_back_off,
        keep_alive,
    );

    let mut default_subscriptions = vec![
        // Twin initial GET response
        mqtt::proto::SubscribeTo {
            topic_filter: "$iothub/twin/res/#".to_string(),
            qos: mqtt::proto::QoS::AtMostOnce,
        },
        // Twin patches
        mqtt::proto::SubscribeTo {
            topic_filter: "$iothub/twin/PATCH/properties/desired/#".to_string(),
            qos: mqtt::proto::QoS::AtMostOnce,
        },
        // Module methods
        mqtt::proto::SubscribeTo {
            topic_filter: "$iothub/methods/POST/#".to_string(),
            qos: mqtt::proto::QoS::AtLeastOnce,
        },
    ];
    if module_id.is_none() {
        default_subscriptions.push(
            // Direct methods
            mqtt::proto::SubscribeTo {
                topic_filter: "$iothub/methods/POST/#".to_string(),
                qos: mqtt::proto::QoS::AtLeastOnce,
            },
        );
    }

    for subscribe_to in default_subscriptions {
        match inner.subscribe(subscribe_to) {
            Ok(()) => (),

            // The subscription can only fail if `inner` has shut down, which is not the case here
            Err(mqtt::UpdateSubscriptionError::ClientDoesNotExist) => unreachable!(),
        }
    }

    Ok(inner)
}

lazy_static::lazy_static! {
    static ref DIRECT_METHOD_REGEX: regex::Regex = regex::Regex::new(r"^\$iothub/methods/POST/([^/]+)/\?\$rid=(.+)$").expect("could not compile regex");
}
