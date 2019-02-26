use std::path::PathBuf;
use std::process::Command;

use failure::Fail;
use futures::Future;
use futures::future::IntoFuture;
use log;
use serde_derive::{Deserialize, Serialize};
use tokio_process::CommandExt;

use crate::error::{Error, ErrorKind};

#[derive(Deserialize, Serialize)]
pub struct Device {
    path: PathBuf,
    num: i8,
    partition: i8,
}

impl Device {
    pub fn new<P: Into<PathBuf>>(path: P, num: i8, partition: i8) -> Self {
        Device {
            path: path.into(),
            num,
            partition,
        }
    }
}

#[derive(Deserialize, Serialize)]
pub struct Updater {
    primary: Device,
    secondary: Device,
}

impl Updater {
    pub fn new(primary: Device, secondary: Device) -> Self {
        Updater {
            primary,
            secondary,
        }
    }

    pub fn reboot(&self) -> impl Future<Item = (), Error = Error> {
        Command::new("/sbin/reboot")
            .status_async()
            .map_err(|e| e.context(ErrorKind::Reboot))
            .into_future()
            .and_then(|child| {
                log::info!("Rebooting...");
                child.map(|status| {
                    log::info!("reboot finished with status {}", status);
                })
                .map_err(|e| e.context(ErrorKind::Reboot))
            })
            .map_err(|e| e.context(ErrorKind::Reboot).into())
    }

    pub fn swap(&mut self) -> impl Future<Item = (), Error = Error> {
        let partition = self.secondary.partition;
        std::mem::swap(&mut self.primary, &mut self.secondary);

        Command::new("/usr/bin/fw_setenv")
            .arg("ota_boot_partition")
            .arg(format!("{}", partition))
            .status_async()
            .map_err(|e| e.context(ErrorKind::Swap))
            .into_future()
            .and_then(|child| {
                log::info!("Swapping partitions...");
                child.map(|status| {
                    log::info!("swap finished with status {}", status);
                })
                .map_err(|e| e.context(ErrorKind::Swap))
            })
            .map_err(|e| e.context(ErrorKind::Swap).into())
    }
}
