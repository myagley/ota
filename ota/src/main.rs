use std::process::Command;
use std::str;
use std::time::Duration;

use azure_iot_mqtt::device;
use futures::{Future, Stream};
use regex::Regex;
use serde_derive::{Deserialize, Serialize};
use serde_json::json;
use tokio::runtime::Runtime;
use tokio_signal;
use url::Url;

mod error;
mod updater;

use crate::updater::{Device, Updater};

#[derive(Deserialize, Serialize)]
pub struct UpdateRequest {
    #[serde(with = "url_serde")]
    url: Url,
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::new().filter_or(
        "AZURE_IOT_OTA_LOG",
        "mqtt=debug,mqtt::logging=trace,azure_iot_mqtt=debug,ota=info",
    ))
    .init();
    let iothub = "miyagley-edge.azure-devices.net";
    let device_id = "raspberrypi3";
    let sas_token = "SharedAccessSignature sr=miyagley-edge.azure-devices.net%2Fdevices%2Fraspberrypi3&sig=0AbCARoU3rONykEauTCY254PilXsaJ6Kl8m5zux%2BA8c%3D&se=1552521063";
    let auth = azure_iot_mqtt::Authentication::SasToken(sas_token.to_string());

    let mut runtime = Runtime::new().expect("couldn't initialize tokio runtime");
    let executor = runtime.executor();

    let re = Regex::new(r"ota_boot_partition=(?P<partition>\d)").expect("regex failed");
    let output = Command::new("/usr/bin/fw_printenv")
        .arg("ota_boot_partition")
        .output()
        .unwrap();
    let caps = re
        .captures(str::from_utf8(&output.stdout).unwrap())
        .unwrap();
    let partition: i8 = caps["partition"].parse().unwrap();
    let mut updater = if partition == 3 {
        let primary = Device::new("/dev/mmcblk0p3", 0, 3);
        let secondary = Device::new("/dev/mmcblk0p2", 0, 2);
        Updater::new(primary, secondary)
    } else {
        let primary = Device::new("/dev/mmcblk0p2", 0, 2);
        let secondary = Device::new("/dev/mmcblk0p3", 0, 3);
        Updater::new(primary, secondary)
    };

    let client = device::Client::new(
        iothub.to_string(),
        device_id,
        auth,
        azure_iot_mqtt::Transport::Tcp,
        None,
        Duration::from_secs(30),
        Duration::from_secs(5),
    )
    .expect("could not create client");

    let shutdown_handle = client
        .inner()
        .shutdown_handle()
        .expect("couldn't get shutdown handle");
    let direct_method_response_handle = client.direct_method_response_handle();

    let shutdown = tokio_signal::ctrl_c()
        .flatten_stream()
        .into_future()
        .then(move |_| {
            log::info!("Shutdown requested...");
            shutdown_handle.shutdown()
        })
        .then(|result| {
            log::info!("Shutdown finished.");
            result.expect("couldn't send shutdown notification");
            Ok(())
        });
    runtime.spawn(shutdown);

    let f = client.for_each(move |message| {
        log::info!("received message {:?}", message);
        if let azure_iot_mqtt::device::Message::DirectMethod {
            name,
            payload,
            request_id,
        } = message
        {
            log::info!(
                "direct method {:?} invoked with payload {:?}",
                name,
                payload
            );
            let handle = direct_method_response_handle.clone();

            match name.as_ref() {
                "reboot" => {
                    log::info!("Received reboot request...");
                    let result = updater
                        .reboot()
                        .then(move |result| match result {
                            Ok(_) => handle.respond(
                                request_id.clone(),
                                azure_iot_mqtt::Status::Ok,
                                json!({"message": "rebooting"}),
                            ),
                            Err(_e) => handle.respond(
                                request_id.clone(),
                                azure_iot_mqtt::Status::BadRequest,
                                payload,
                            ),
                        })
                        .then(move |result| {
                            let () = result.expect("couldn't send direct method response");
                            log::info!("Rebooting finished and responded to request");
                            Ok(())
                        });
                    executor.spawn(result)
                }
                "swap" => {
                    log::info!("Received swap request...");
                    let result = updater
                        .swap()
                        .then(move |result| match result {
                            Ok(_) => handle.respond(
                                request_id.clone(),
                                azure_iot_mqtt::Status::Ok,
                                json!({"message": "swapped"}),
                            ),
                            Err(_e) => handle.respond(
                                request_id.clone(),
                                azure_iot_mqtt::Status::BadRequest,
                                payload,
                            ),
                        })
                        .then(move |result| {
                            let () = result.expect("couldn't send direct method response");
                            log::info!("Swapping finished and responded to request");
                            Ok(())
                        });
                    executor.spawn(result)
                }
                "load" => {
                    log::info!("Received load request...");
                    let request: UpdateRequest =
                        serde_json::from_value(payload).expect("failed to parse request");
                    let result = updater
                        .load(request.url)
                        .then(move |result| match result {
                            Ok(_) => handle.respond(
                                request_id.clone(),
                                azure_iot_mqtt::Status::Ok,
                                json!({"message": "loaded"}),
                            ),
                            Err(e) => handle.respond(
                                request_id.clone(),
                                azure_iot_mqtt::Status::BadRequest,
                                json!({"message": e.to_string()}),
                            ),
                        })
                        .then(move |result| {
                            let () = result.expect("couldn't send direct method response");
                            log::info!("Rebooting finished and responded to request");
                            Ok(())
                        });
                    executor.spawn(result)
                }
                _ => {
                    // Respond with status 200 and same payload
                    let result = handle
                        .respond(request_id.clone(), azure_iot_mqtt::Status::Ok, payload)
                        .then(move |result| {
                            let () = result.expect("couldn't send direct method response");
                            log::info!("Responded to request {}", request_id);
                            Ok(())
                        });
                    executor.spawn(result)
                }
            };
        }

        Ok(())
    });

    runtime.block_on(f).expect("azure-iot-mqtt-client failed");
}
