use std::fs;
use std::io::Write;
use std::mem;
use std::path::PathBuf;
use std::process::Command;

use failure::Fail;
use futures::future::IntoFuture;
use futures::{Future, Stream};
use log;
use reqwest::r#async::{Client, Decoder};
use reqwest::IntoUrl;
use serde_derive::{Deserialize, Serialize};
use tokio_fs::file::File;
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
        Updater { primary, secondary }
    }

    pub fn reboot(&self) -> impl Future<Item = (), Error = Error> {
        Command::new("/sbin/reboot")
            .status_async()
            .map_err(|e| e.context(ErrorKind::Reboot))
            .into_future()
            .and_then(|child| {
                log::info!("Rebooting...");
                child
                    .map(|status| {
                        log::info!("reboot finished with status {}", status);
                    })
                    .map_err(|e| e.context(ErrorKind::Reboot))
            })
            .map_err(|e| e.context(ErrorKind::Reboot).into())
    }

    pub fn swap(&mut self) -> impl Future<Item = (), Error = Error> {
        let partition = self.secondary.partition;
        std::mem::swap(&mut self.primary, &mut self.secondary);

        Command::new("/sbin/fw_setenv")
            .arg("ota_boot_partition")
            .arg(format!("{}", partition))
            .status_async()
            .map_err(|e| e.context(ErrorKind::Swap))
            .into_future()
            .and_then(|child| {
                log::info!("Swapping partitions...");
                child
                    .map(|status| {
                        log::info!("swap finished with status {}", status);
                    })
                    .map_err(|e| e.context(ErrorKind::Swap))
            })
            .map_err(|e| e.context(ErrorKind::Swap).into())
    }

    pub fn load<I: IntoUrl>(&self, url: I) -> impl Future<Item = (), Error = Error> {
        let u = url.into_url().unwrap();
        log::info!("Loading {} into {:?}", u, self.secondary.path);
        let device = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.secondary.path)
            .unwrap();
        let mut file = File::from_std(device);
        Client::new()
            .get(u)
            .send()
            .map_err(|e| e.context(ErrorKind::Download))
            .and_then(move |mut res| {
                log::info!("Download status: {}", res.status());
                let mut chunks = 0;
                let mut bytes = 0;
                let body = mem::replace(res.body_mut(), Decoder::empty());
                body.map_err(|e| e.context(ErrorKind::Download))
                    .for_each(move |chunk| {
                        bytes = bytes + chunk.len();
                        chunks = chunks + 1;
                        if chunks % 100 == 0 {
                            log::info!("Progress - {}", bytes);
                        }
                        file.write_all(&chunk)
                            .map_err(|e| e.context(ErrorKind::Download))
                    })
                    .map_err(|e| e.context(ErrorKind::Download))
            })
            .map_err(|e| e.context(ErrorKind::Download).into())
    }
}
