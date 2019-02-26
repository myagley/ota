use std::process::Command;

use failure::Fail;
use futures::Future;
use futures::future::IntoFuture;
use log;
use tokio_process::CommandExt;

use crate::error::{Error, ErrorKind};

pub struct Updater;

impl Updater {
    pub fn new() -> Self {
        Updater
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
}
