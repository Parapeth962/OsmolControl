//! DirCon (Wahoo Direct Connect) server: a virtual BLE device over LAN TCP. See docs/protocol/dircon.md.
use anyhow::{Context, Result, bail};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::{collections::HashSet, net::Ipv4Addr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{broadcast, mpsc, watch},
    task::JoinSet,
};
use uuid::Uuid;

pub mod codec;
pub mod gatt;
pub use gatt::{Characteristic, Gatt, Service, sig};

const SERVICE: &str = "_wahoo-fitness-tnp._tcp.local.";
const SERVICE_NAME_LEN: u8 = 18;
const MAX_CLIENTS: usize = 4;

#[derive(Debug, Clone)]
pub struct Config {
    pub address: Ipv4Addr,
    pub port: u16,
    /// Shown in the training app's device list.
    pub name: String,
    pub serial: String,
    pub advertise: bool,
}

/// Values pushed to subscribed clients: (characteristic, value).
pub type Notifications = broadcast::Sender<(Uuid, Vec<u8>)>;
/// Client writes to writable characteristics: (characteristic, data).
pub type Writes = mpsc::Sender<(Uuid, Vec<u8>)>;

/// Stable, locally administered pseudo MAC; DirCon clients expect one in the TXT record.
fn pseudo_mac(seed: &str) -> String {
    let hash = seed.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    });
    let b = hash.to_be_bytes();
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        (b[0] & 0xfc) | 0x02,
        b[1],
        b[2],
        b[3],
        b[4],
        b[5]
    )
}

async fn write(socket: &mut TcpStream, packet: &codec::Packet) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(3), socket.write_all(&packet.encode()))
        .await
        .context("slow DirCon client")??;
    Ok(())
}

async fn client(
    mut socket: TcpStream,
    gatt: Arc<Gatt>,
    mut notifications: broadcast::Receiver<(Uuid, Vec<u8>)>,
    writes: Writes,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    let peer = socket.peer_addr().ok();
    tracing::info!(?peer, "DirCon client connected");
    let mut decoder = codec::Decoder::default();
    let mut subscriptions = HashSet::new();
    let mut bytes = [0u8; 1024];
    // Nothing is sent until the client asks: an early frame makes MyWhoosh drop 2AD2.
    loop {
        tokio::select! {
            _ = stop.changed() => return Ok(()),
            n = socket.read(&mut bytes) => {
                let n = n?;
                if n == 0 {
                    tracing::info!(?peer, "DirCon client disconnected");
                    return Ok(());
                }
                for request in decoder.push(&bytes[..n])? {
                    let (reply, effect) = gatt.handle(&request, &mut subscriptions);
                    write(&mut socket, &reply).await?;
                    if let gatt::Effect::Write(uuid, data) = effect
                        && writes.try_send((uuid, data)).is_err()
                    {
                        tracing::warn!("DirCon write dropped: consumer busy");
                    }
                }
            }
            n = notifications.recv() => match n {
                Ok((uuid, value)) if subscriptions.contains(&uuid) => {
                    write(&mut socket, &gatt::notification(uuid, &value)).await?;
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::debug!(skipped, "DirCon notifications skipped for slow client");
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
        }
    }
}

pub async fn serve(
    config: Config,
    gatt: Gatt,
    notifications: Notifications,
    writes: Writes,
    mut stop: watch::Receiver<bool>,
    active: watch::Sender<usize>,
) -> Result<()> {
    if config.address.is_unspecified() || config.address.is_multicast() {
        bail!("choose a concrete LAN IPv4 address");
    }
    let listener = TcpListener::bind((config.address, config.port))
        .await
        .context("bind DirCon TCP server")?;
    let port = listener.local_addr()?.port();
    let advertisement = if config.advertise {
        let daemon = ServiceDaemon::new().context("start mDNS responder")?;
        // "_wahoo-fitness-tnp" is 18 bytes, above RFC 6763's 15; mdns-sd rejects it by default.
        daemon
            .set_service_name_len_max(SERVICE_NAME_LEN)
            .context("allow DirCon service name")?;
        let errors = daemon.monitor().context("monitor mDNS responder")?;
        std::thread::spawn(move || {
            while let Ok(event) = errors.recv() {
                if let mdns_sd::DaemonEvent::Error(e) = event {
                    tracing::error!(error = %e, "DirCon mDNS advertisement failed");
                }
            }
        });
        let uuids = gatt
            .services
            .iter()
            .map(|s| s.uuid.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let mac = pseudo_mac(&format!("{}/{}", config.name, config.serial));
        let txt = [
            ("mac-address", mac.as_str()),
            ("serial-number", config.serial.as_str()),
            ("ble-service-uuids", uuids.as_str()),
        ];
        // IPv4-only hostname derived from the display name, as DirCon clients expect.
        let host = format!("{}.local.", config.name.replace(' ', "-"));
        let info = ServiceInfo::new(
            SERVICE,
            &config.name,
            &host,
            std::net::IpAddr::V4(config.address),
            port,
            &txt[..],
        )?;
        let fullname = info.get_fullname().to_owned();
        daemon.register(info)?;
        Some((daemon, fullname))
    } else {
        None
    };
    tracing::info!(address = %config.address, port, name = %config.name, "DirCon trainer ready");
    let gatt = Arc::new(gatt);
    let mut clients = JoinSet::new();
    let result: Result<()> = loop {
        if *stop.borrow() {
            break Ok(());
        }
        tokio::select! {
            _ = stop.changed() => break Ok(()),
            accepted = listener.accept() => match accepted {
                Ok((socket, _)) if clients.len() < MAX_CLIENTS => {
                    clients.spawn(client(socket, gatt.clone(), notifications.subscribe(), writes.clone(), stop.clone()));
                    let _ = active.send(clients.len());
                }
                Ok(_) => tracing::warn!("DirCon client limit reached"),
                Err(e) => break Err(e.into()),
            },
            result = clients.join_next(), if !clients.is_empty() => {
                if let Some(Ok(Err(e))) = result {
                    tracing::warn!(error = %e, "DirCon client error");
                }
                let _ = active.send(clients.len());
            }
        }
    };
    clients.abort_all();
    while clients.join_next().await.is_some() {}
    if let Some((daemon, name)) = advertisement {
        let _ = daemon.unregister(&name);
        let _ = daemon.shutdown();
    }
    result
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "loopback test setup")]
mod tests {
    use super::*;

    #[test]
    fn mac_is_stable_and_locally_administered() {
        let a = pseudo_mac("OsmolControl/1");
        assert_eq!(a, pseudo_mac("OsmolControl/1"));
        assert_eq!(a.len(), 17);
        let first = u8::from_str_radix(&a[..2], 16).unwrap();
        assert_eq!(first & 0x03, 0x02);
    }

    #[tokio::test]
    async fn loopback_session_subscribe_write_notify() {
        let gatt = Gatt {
            services: vec![Service {
                uuid: sig(0x1826),
                characteristics: vec![
                    Characteristic {
                        uuid: sig(0x2ad2),
                        properties: gatt::NOTIFY,
                        value: vec![],
                    },
                    Characteristic {
                        uuid: sig(0x2ad9),
                        properties: gatt::WRITE | gatt::INDICATE,
                        value: vec![],
                    },
                ],
            }],
        };
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let (notify, _) = broadcast::channel(8);
        let (writes, mut written) = mpsc::channel(8);
        let (stop, stopped) = watch::channel(false);
        let config = Config {
            address: Ipv4Addr::LOCALHOST,
            port,
            name: "Test".into(),
            serial: "1".into(),
            advertise: false,
        };
        let (active, _) = watch::channel(0);
        let server = tokio::spawn(serve(config, gatt, notify.clone(), writes, stopped, active));
        let mut s = loop {
            if let Ok(s) = TcpStream::connect(("127.0.0.1", port)).await {
                break s;
            }
            tokio::task::yield_now().await;
        };
        let request = |id: u8, seq: u8, payload: Vec<u8>| {
            codec::Packet {
                id,
                seq,
                code: 0,
                payload,
            }
            .encode()
        };
        let mut sub = sig(0x2ad2).as_bytes().to_vec();
        sub.push(1);
        s.write_all(&request(codec::ENABLE_NOTIFICATIONS, 5, sub))
            .await
            .unwrap();
        let mut reply = [0u8; 22];
        s.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply[..6], [1, codec::ENABLE_NOTIFICATIONS, 5, 0, 0, 16]);
        let mut cmd = sig(0x2ad9).as_bytes().to_vec();
        cmd.push(0x00);
        s.write_all(&request(codec::WRITE, 6, cmd)).await.unwrap();
        s.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply[..4], [1, codec::WRITE, 6, 0]);
        assert_eq!(written.recv().await, Some((sig(0x2ad9), vec![0x00])));
        // Unsubscribed characteristics are not forwarded; subscribed ones are.
        notify.send((sig(0x2ad9), vec![0x80, 0, 1])).unwrap();
        notify.send((sig(0x2ad2), vec![0x44, 0])).unwrap();
        let mut n = [0u8; 24];
        s.read_exact(&mut n).await.unwrap();
        assert_eq!(n[..6], [1, codec::NOTIFICATION, 0, 0, 0, 18]);
        assert_eq!(n[22..], [0x44, 0]);
        stop.send(true).unwrap();
        server.await.unwrap().unwrap();
    }
}
