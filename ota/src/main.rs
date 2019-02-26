use std::time::Duration;

use azure_iot_mqtt::device;
use futures::{Future, Stream};
use tokio::runtime::Runtime;
use tokio_signal;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::new().filter_or("AZURE_IOT_OTA_LOG", "mqtt=debug,mqtt::logging=trace,azure_iot_mqtt=debug,ota=info")).init();
    let iothub = "miyagley-edge.azure-devices.net";
    let device_id = "raspberrypi3";
    let sas_token = "SharedAccessSignature sr=miyagley-edge.azure-devices.net%2Fdevices%2Fraspberrypi3&sig=0AbCARoU3rONykEauTCY254PilXsaJ6Kl8m5zux%2BA8c%3D&se=1552521063";
    let auth = azure_iot_mqtt::Authentication::SasToken(sas_token.to_string());

    let mut runtime = Runtime::new().expect("couldn't initialize tokio runtime");
    let executor = runtime.executor();

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
            result.expect("couldn't send shutodown notification");
            Ok(())
        });
    runtime.spawn(shutdown);

    let f = client.for_each(move |message| {
        log::info!("received message {:?}", message);
        if let azure_iot_mqtt::device::Message::DirectMethod { name, payload, request_id } = message {
            log::info!("direct method {:?} invoked with payload {:?}", name, payload);

            // Respond with status 200 and same payload
            executor.spawn(direct_method_response_handle
                .respond(request_id.clone(), azure_iot_mqtt::Status::Ok, payload)
                .then(move |result| {
                    let () = result.expect("couldn't send direct method response");
                    log::info!("Responded to request {}", request_id);
                    Ok(())
                }));
        }

        Ok(())
    });

    runtime.block_on(f).expect("azure-iot-mqtt-client failed");
}
