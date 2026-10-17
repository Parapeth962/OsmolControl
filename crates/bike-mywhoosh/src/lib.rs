//! OpenBikeControl TCP server with mDNS discovery; controller actions only.
use anyhow::{Context, Result, bail};
use bike_protocol::{Action, AppDecoder, AppInfo, AppMessage};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::{
    net::Ipv4Addr,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{broadcast, watch},
    task::JoinSet,
};
use uuid::Uuid;

pub const SERVICE: &str = "_openbikecontrol._tcp.local.";
pub const SERVICE_UUID: &str = "d273f680-d548-419d-b9d1-fa0472345229";
#[derive(Debug, Clone)]
pub struct Config {
    pub address: Ipv4Addr,
    pub port: u16,
    pub name: String,
    pub advertise: bool,
}
async fn write(socket: &mut TcpStream, bytes: &[u8]) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(3), socket.write_all(bytes))
        .await
        .context("slow MyWhoosh client")??;
    Ok(())
}
async fn client(
    mut socket: TcpStream,
    mut actions: broadcast::Receiver<(Instant, Action)>,
    mut stop: watch::Receiver<bool>,
    mut readiness: watch::Receiver<bool>,
) -> Result<()> {
    socket.set_nodelay(true)?;
    let mut decoder = AppDecoder::default();
    let mut app: Option<AppInfo> = None;
    let mut bytes = [0u8; 1024];
    let mut status = tokio::time::interval(Duration::from_secs(30));
    let mut pending_since: Option<Instant> = None;
    let mut housekeeping = tokio::time::interval(Duration::from_secs(1));
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        tokio::select! {
            _=stop.changed()=>return Ok(()),
            n=socket.read(&mut bytes)=>{
                let n=n?;if n==0 {return Ok(());}
                for message in decoder.push(&bytes[..n])?{match message{
                    AppMessage::Info(info)=>{tracing::info!(app=%info.id,version=%info.version,"app capabilities received");app=Some(info);}
                    AppMessage::Haptic{..}=>tracing::debug!("haptic request ignored: no verified haptic device"),
                    // MyWhoosh 5.7.0 sends a lone 0x00 every 4 s; treated as a keepalive.
                    AppMessage::Unknown(raw) if raw==[0]=>tracing::trace!("app keepalive"),
                    AppMessage::Unknown(raw)=>tracing::warn!(raw=%bike_protocol::hex(&raw),"unknown app-to-device message dropped"),
                }}
                if decoder.is_pending(){pending_since.get_or_insert_with(Instant::now);}else{pending_since=None;}
            }
            event=actions.recv()=>match event{
                Ok((at,action))=>{
                    if at.elapsed()>Duration::from_millis(500) || !*readiness.borrow(){tracing::debug!(?action,"stale or not-ready action dropped");continue;}
                    if app.as_ref().is_none_or(|a|a.supports(action)){
                        write(&mut socket,&action.transition(true)).await?;
                        write(&mut socket,&action.transition(false)).await?;
                    }else{tracing::warn!(?action,"app does not support action");}
                }
                Err(broadcast::error::RecvError::Lagged(_))=>{bail!("action consumer lagged; reconnect to avoid partial input");}
                Err(broadcast::error::RecvError::Closed)=>return Ok(()),
            },
            _=status.tick()=>{let ready=*readiness.borrow();write(&mut socket,&[2,255,u8::from(ready)]).await?;},
            changed=readiness.changed()=>{if changed.is_err(){return Ok(());}let ready=*readiness.borrow();write(&mut socket,&[2,255,u8::from(ready)]).await?;},
            _=housekeeping.tick()=>{if pending_since.is_some_and(|at|at.elapsed()>Duration::from_secs(5)){bail!("incomplete app frame timed out");}}
        }
    }
}
/// At most four active clients. New sessions get no historic button events.
pub async fn serve_with_readiness(
    config: Config,
    actions: broadcast::Sender<(Instant, Action)>,
    stop: watch::Receiver<bool>,
    readiness: watch::Receiver<bool>,
) -> Result<()> {
    serve_with_clients(config, actions, stop, readiness, watch::channel(0).0).await
}
/// As [`serve_with_readiness`], also publishing the number of connected apps.
pub async fn serve_with_clients(
    config: Config,
    actions: broadcast::Sender<(Instant, Action)>,
    mut stop: watch::Receiver<bool>,
    readiness: watch::Receiver<bool>,
    active: watch::Sender<usize>,
) -> Result<()> {
    if config.address.is_unspecified() || config.address.is_multicast() {
        bail!("choose a concrete LAN IPv4 address");
    }
    let listener = TcpListener::bind((config.address, config.port))
        .await
        .context("bind OpenBikeControl TCP server")?;
    let port = listener.local_addr()?.port();
    let id = Uuid::new_v4().simple().to_string();
    let advertisement = if config.advertise {
        let daemon = ServiceDaemon::new().context("start mDNS responder")?;
        let txt = [
            ("version", "1"),
            ("id", id.as_str()),
            ("name", config.name.as_str()),
            ("service-uuids", SERVICE_UUID),
            ("manufacturer", config.name.as_str()),
            ("model", "bikectl"),
        ];
        let info = ServiceInfo::new(
            SERVICE,
            &config.name,
            &format!("osmolcontrol-{}.local.", &id[..12]),
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
    tracing::info!(address=%config.address,port,"OpenBikeControl ready; select this controller in MyWhoosh");
    let mut clients = JoinSet::new();
    let result: Result<()> = loop {
        if *stop.borrow() {
            break Ok(());
        }
        tokio::select! {
            _=stop.changed()=>break Ok(()),
            accepted=listener.accept()=>match accepted{
                Ok((socket,_peer))=>{if clients.len()<4 {clients.spawn(client(socket,actions.subscribe(),stop.clone(),readiness.clone()));let _=active.send(clients.len());}else{tracing::warn!("client limit reached");}}
                Err(e)=>break Err(e.into()),
            },
            result=clients.join_next(),if !clients.is_empty()=>{match result{Some(Ok(Err(e)))=>tracing::warn!(error=%e,"client disconnected"),Some(Err(e))=>tracing::warn!(error=%e,"client task failed"),_=>{}}let _=active.send(clients.len());}
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

/// Serve an already-ready input source. BLE callers should use `serve_with_readiness`.
pub async fn serve(
    config: Config,
    actions: broadcast::Sender<(Instant, Action)>,
    stop: watch::Receiver<bool>,
) -> Result<()> {
    let (_sender, ready) = watch::channel(true);
    serve_with_readiness(config, actions, stop, ready).await
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "test setup and assertions")]
mod tests {
    use super::*;
    #[tokio::test]
    async fn real_tcp_fragmentation_capabilities_and_no_stale_actions() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut peer = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (socket, _) = listener.accept().await.unwrap();
        let (tx, _) = broadcast::channel(8);
        let (stop, rx) = watch::channel(false);
        let (_ready, readiness) = watch::channel(true);
        let worker = tokio::spawn(client(socket, tx.subscribe(), rx, readiness));
        let mut status = [0; 3];
        peer.read_exact(&mut status).await.unwrap();
        assert_eq!(status, [2, 255, 1]);
        // Fragmented app info, only shift-up allowed, followed by haptic frame.
        peer.write_all(&[4, 1, 1]).await.unwrap();
        peer.write_all(&[b'x', 0, 1, 1, 3, 0, 0, 0]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        tx.send((Instant::now() - Duration::from_secs(2), Action::ShiftUp))
            .unwrap();
        tx.send((Instant::now(), Action::ShiftDown)).unwrap();
        tx.send((Instant::now(), Action::ShiftUp)).unwrap();
        let mut transitions = [0; 6];
        tokio::time::timeout(Duration::from_secs(1), peer.read_exact(&mut transitions))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(transitions, [1, 1, 1, 1, 1, 0]);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), peer.read_u8())
                .await
                .is_err()
        );
        stop.send(true).unwrap();
        worker.await.unwrap().unwrap();
    }
    #[tokio::test]
    async fn optional_app_info_and_zero_count() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut peer = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (socket, _) = listener.accept().await.unwrap();
        let (tx, _) = broadcast::channel(8);
        let (stop, rx) = watch::channel(false);
        let (_ready, readiness) = watch::channel(true);
        let worker = tokio::spawn(client(socket, tx.subscribe(), rx, readiness));
        let mut b = [0; 3];
        peer.read_exact(&mut b).await.unwrap();
        peer.write_all(&[4, 1, 0, 0, 0]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        tx.send((Instant::now(), Action::ShiftDown)).unwrap();
        let mut b = [0; 6];
        peer.read_exact(&mut b).await.unwrap();
        assert_eq!(b, [1, 2, 1, 1, 2, 0]);
        stop.send(true).unwrap();
        worker.await.unwrap().unwrap();
    }
    #[tokio::test]
    async fn malformed_peer_closes_session() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut peer = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (socket, _) = listener.accept().await.unwrap();
        let (tx, _) = broadcast::channel(8);
        let (_stop, rx) = watch::channel(false);
        let (_ready, readiness) = watch::channel(true);
        let worker = tokio::spawn(client(socket, tx.subscribe(), rx, readiness));
        peer.write_all(&[4, 2]).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), worker)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
    }
    #[tokio::test]
    async fn not_ready_suppresses_actions_and_reports_status() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut peer = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (socket, _) = listener.accept().await.unwrap();
        let (tx, _) = broadcast::channel(8);
        let (stop, rx) = watch::channel(false);
        let (ready, readiness) = watch::channel(false);
        let worker = tokio::spawn(client(socket, tx.subscribe(), rx, readiness));
        let mut b = [0; 3];
        peer.read_exact(&mut b).await.unwrap();
        assert_eq!(b, [2, 255, 0]);
        tx.send((Instant::now(), Action::ShiftUp)).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(30), peer.read_u8())
                .await
                .is_err()
        );
        ready.send(true).unwrap();
        peer.read_exact(&mut b).await.unwrap();
        assert_eq!(b, [2, 255, 1]);
        stop.send(true).unwrap();
        worker.await.unwrap().unwrap();
    }
}
