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
        Command::new("reboot")
            .spawn_async()
            .into_future()
            .map(|_child| {
                log::info!("Rebooting...");
            })
            .map_err(|e| e.context(ErrorKind::Reboot).into())
    }
}
