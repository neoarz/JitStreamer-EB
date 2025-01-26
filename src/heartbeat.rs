// Jackson Coxson
// Orchestrator for heartbeat threads

use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
};

use idevice::{
    heartbeat::HeartbeatClient, lockdownd::LockdowndClient, pairing_file::PairingFile, Idevice,
    IdeviceError,
};
use log::debug;
use tokio::sync::oneshot::error::TryRecvError;

pub enum SendRequest {
    Store((String, tokio::sync::oneshot::Sender<()>)),
    Kill(String),
}
pub type NewHeartbeatSender = tokio::sync::mpsc::Sender<SendRequest>;

pub fn heartbeat() -> NewHeartbeatSender {
    let (sender, mut receiver) = tokio::sync::mpsc::channel::<SendRequest>(100);
    tokio::task::spawn(async move {
        let mut cache: HashMap<String, tokio::sync::oneshot::Sender<()>> = HashMap::new();
        while let Some(msg) = receiver.recv().await {
            match msg {
                SendRequest::Store((udid, handle)) => {
                    if let Some(old_sender) = cache.insert(udid, handle) {
                        old_sender.send(()).ok();
                    }
                }
                SendRequest::Kill(udid) => {
                    if let Some(old_sender) = cache.remove(&udid) {
                        old_sender.send(()).ok();
                    }
                }
            }
        }
    });
    sender
}

pub async fn heartbeat_thread(
    udid: String,
    ip: IpAddr,
    pairing_file: &PairingFile,
) -> Result<tokio::sync::oneshot::Sender<()>, IdeviceError> {
    debug!("Connecting to device {udid} to get apps");
    let socket = SocketAddr::new(ip, idevice::lockdownd::LOCKDOWND_PORT);

    let socket = tokio::net::TcpStream::connect(socket).await?;
    let socket = Box::new(socket);
    let idevice = Idevice::new(socket, "JitStreamer");

    let mut lockdown_client = LockdowndClient { idevice };
    lockdown_client.start_session(pairing_file).await?;

    let (port, _) = lockdown_client
        .start_service("com.apple.mobile.heartbeat")
        .await?;

    let socket = SocketAddr::new(ip, port);
    let socket = tokio::net::TcpStream::connect(socket).await?;

    let socket = Box::new(socket);
    let mut idevice = Idevice::new(socket, "JitStreamer");

    idevice.start_session(pairing_file).await?;

    let mut heartbeat_client = HeartbeatClient { idevice };

    let (sender, mut receiver) = tokio::sync::oneshot::channel::<()>();

    tokio::task::spawn(async move {
        let mut interval = 15;
        loop {
            interval = match heartbeat_client.get_marco(interval).await {
                Ok(interval) => interval,
                Err(_) => break,
            };
            if heartbeat_client.send_polo().await.is_err() {
                break;
            }
            match receiver.try_recv() {
                Ok(_) => break,
                Err(TryRecvError::Closed) => break,
                Err(TryRecvError::Empty) => {}
            }
        }
    });
    Ok(sender)
}
