use std::collections::HashMap;
use std::fmt;
use std::fmt::{Display, Formatter};
use std::time::Duration;
use tokio::sync::mpsc::{
    self,
    error::{TrySendError, TryRecvError}
};
use tokio::task::JoinHandle;
use futures_util::{stream, StreamExt};
use tokio::{select, time};
use tokio_util::sync::CancellationToken;
use crate::lobby::room_info::{RoomID, RoomInfo};
use crate::netconnection::{NetConnection, NetConnectionMessage, NetConnectionRead, NetConnectionSink, NETID, NETID_ALL, NETID_NONE, NETID_RELAY};
use crate::relay::messages::{NetRelayMessagePeer, NetRelayMessageRelay};
use crate::utils::config_from_str;
use crate::utils::time::IntervalTimer;

#[derive(Clone, Debug)]
pub struct PeerRelayStatus {
    pub peers: usize,
    pub sinks: usize,
    pub pings: HashMap<NETID, u32>,
    pub info: Option<RoomInfo>,
}

impl PeerRelayStatus {
    pub fn new() -> Self {
        Self {
            peers: 0,
            sinks: 0,
            pings: HashMap::new(),
            info: None,
        }
    }
}

#[derive(Debug)]
pub enum PeerRelayCloseMode {
    NoPeers,
    NoSinks,
}

#[derive(Debug)]
pub enum PeerRelayOwnership {
    None,
    Peer(NETID),
    #[allow(dead_code)]
    All,
}

#[derive(Debug)]
pub enum PeerRelayMessage {
    StoreConnection(NetConnection),
    StoreSink(NetConnectionSink),
    #[allow(dead_code)]
    RemoveConnection(NETID),
    #[allow(dead_code)]
    RemoveSink(NETID),
    UpdateStatus(PeerRelayStatus),
    #[allow(dead_code)]
    SetOwnership(PeerRelayOwnership),
}

#[derive(Debug, Clone)]
pub struct PeerRelayConfig {
    ///Room ID
    pub room_id: RoomID,
    ///Interval for update loop
    pub update_interval: Duration,
    ///How many messages to poll per peer on each update
    pub msgs_per_poll: usize,
    ///Timer for status update
    pub status_timer: IntervalTimer,
    ///Timer for ping
    pub ping_timer: IntervalTimer,
    ///How long to wait before pinging
    pub ping_wait_time: Duration,
    ///Token to cancel task
    pub cancellation: CancellationToken,
    ///Max amount of messages to poll from channels on each update
    pub channel_msgs_per_update: usize,
}

impl PeerRelayConfig {
    pub fn new(room_id: RoomID) -> Self {
        let update_interval = config_from_str::<u64>("SERVER_ROOM_UPDATE_INTERVAL", 10);
        let status_interval = config_from_str::<u64>("SERVER_ROOM_STATUS_INTERVAL", 1000);
        let ping_interval = config_from_str::<u64>("SERVER_ROOM_PING_POLL_INTERVAL", 3000);
        let ping_wait_time = config_from_str::<u64>("SERVER_ROOM_PING_WAIT_TIME", ping_interval + 1000);
        Self {
            room_id,
            update_interval: Duration::from_millis(update_interval),
            channel_msgs_per_update: config_from_str::<usize>("SERVER_ROOM_TASK_CHANNEL_MSGS_PER_UPDATE", 10),
            msgs_per_poll: config_from_str::<usize>("SERVER_ROOM_MESSAGES_PER_POLL", 10),
            status_timer: IntervalTimer::new(Duration::from_millis(status_interval)),
            ping_timer: IntervalTimer::new(Duration::from_millis(ping_interval)),
            ping_wait_time: Duration::from_millis(ping_wait_time),
            cancellation: CancellationToken::new(),
        }
    }
}

pub struct PeerRelay {
    ///Config for room
    config: PeerRelayConfig,
    ///Communication channel to room
    room_tx: mpsc::Sender<PeerRelayMessage>,
    ///Communication channel from room
    room_rx: mpsc::Receiver<PeerRelayMessage>,
    ///Close mode
    close_mode: PeerRelayCloseMode,
    ///Ownership of room
    ownership: PeerRelayOwnership,
    ///Connections that are used as stream
    peers: HashMap<NETID, NetConnection>,
    ///Map of client peers to send to
    sinks: HashMap<NETID, NetConnectionSink>,
    ///Most recent status info to send
    status: PeerRelayStatus,
}

impl PeerRelay {
    pub fn new(
        config: PeerRelayConfig,
        room_tx: mpsc::Sender<PeerRelayMessage>,
        room_rx: mpsc::Receiver<PeerRelayMessage>,
        close_mode: PeerRelayCloseMode,
        ownership: PeerRelayOwnership,
        starting_peers: Vec<NetConnection>,
        starting_sinks: Vec<NetConnectionSink>,
    ) -> Self {
        let peers = HashMap::from_iter(
            starting_peers.into_iter().map(|peer| (peer.get_netid(), peer))
        );
        let sinks = HashMap::from_iter(
            starting_sinks.into_iter().map(|sink| (sink.get_netid(), sink))
        );
        Self {
            config,
            room_tx,
            room_rx,
            close_mode,
            ownership,
            peers,
            sinks,
            status: PeerRelayStatus::new(),
        }
    }
    
    pub fn run(self) -> JoinHandle<()> {
        tokio::spawn(self.entry())
    }
    
    async fn entry(mut self) {
        //This avoids room being removed due to being "empty" with initial peers/sinks
        self.update_status();

        //Notify the initial peers
        for conn in self.peers.values() {
            self.send_list_peers(conn.sink()).await;
        }
        
        //Call main loop with cancellation token
        let cancelation = self.config.cancellation.clone();
        select! {
            _ = cancelation.cancelled() => {},
            _ = self.main_loop() => {},
        }
        
        //Last update and close
        self.update_status();
        self.close();
    }
    
    async fn main_loop(&mut self) {
        let mut interval = time::interval(self.config.update_interval);
        while !self.is_closed() {
            if self.config.status_timer.tick() {
                self.update_status();
            }
            self.poll_peers().await;
            if self.config.ping_timer.tick() {
                self.ping_peers().await;
            }
            self.process_queue().await;
            self.check_close_state();
            
            //Eepy time, also important to let this loop don't block the async
            interval.tick().await;
        }
    }
    
    fn is_closed(&self) -> bool {
        self.config.cancellation.is_cancelled()
    }
    
    fn close(&mut self) {
        if !self.is_closed() {
            log::debug!("{:} shutdown", self);
        }
        self.config.cancellation.cancel();
        self.room_rx.close();
        for conn in self.peers.values_mut() {
            conn.close(971382753);
        }
        self.peers.clear();
        self.sinks.clear();
        self.status.pings.clear();
    }
    
    async fn send_list_peers(&self, sink: &NetConnectionSink) {
        //Send list of current peers
        if let Err(err) = sink.send_relay_message(
            NetRelayMessageRelay::ListPeers(
                self.sinks.keys().map(NETID::to_owned).collect()
            ), false
        ).await {
            log::error!("{:} sending list peers to {:} error: {:}", self, sink, err);
        }
    }

    async fn handle_store_connection(&mut self, mut conn: NetConnection) {
        log::debug!("{:} handle_store_connection {:}", self, conn);
        if self.peers.contains_key(&conn.get_netid()) {
            log::debug!("{:} handle_store_connection {:} already added", self, conn.get_netid());
            return;
        }
        self.send_list_peers(conn.sink()).await;
        conn.stream_mut().update_last_contact();
        self.peers.insert(conn.get_netid(), conn);
    }

    async fn handle_remove_connection(&mut self, netid: NETID) {
        log::debug!("{:} handle_remove_connection {:}", self, netid);
        if let None = self.peers.remove(&netid) {
            //Nothing was removed
            log::debug!("{:} handle_remove_connection {:} isn't present", self, netid);
            return;
        }
    }

    async fn handle_store_sink(&mut self, sink: NetConnectionSink) {
        let netid = sink.get_netid();
        if self.sinks.contains_key(&netid) {
            log::debug!("{:} handle_store_sink {:} already added", self, netid);
            return;
        }
        log::debug!("{:} handle_store_sink {:}", self, netid);
        self.sinks.insert(netid, sink);

        //Notify peers about it 
        let self_str = self.to_string();
        for conn in self.peers.values_mut() {
            if let Err(err) = conn.sink().send_relay_message(
                NetRelayMessageRelay::AddPeer(netid), false
            ).await {
                log::error!("{:} sending sink {:} addition to peer {:} error: {:}", self_str, netid, conn, err);
            }
        }
    }

    async fn handle_remove_sink(&mut self, netid: NETID) {
        log::debug!("{:} handle_remove_sink {:}", self, netid);
        if let None = self.sinks.remove(&netid) {
            //Nothing was removed
            log::debug!("{:} handle_remove_sink {:} isn't present", self, netid);
            return;
        }

        //Notify peers about it 
        let self_str = self.to_string();
        for conn in self.peers.values_mut() {
            if conn.is_closed() {
                continue;
            }
            if let Err(err) = conn.sink().send_relay_message(
                NetRelayMessageRelay::RemovePeer(netid), false
            ).await {
                log::error!("{:} sending sink {:} removal to peer {:} error: {:}", self_str, netid, conn, err);
            }
        }
    }

    fn has_permission(&self, netid: NETID) -> bool {
        match self.ownership {
            PeerRelayOwnership::None => false,
            PeerRelayOwnership::All => true,
            PeerRelayOwnership::Peer(owner) => netid == owner,
        }
    }

    fn update_status(&mut self) {
        //Remove pings from peers that no longer exist
        self.status.peers = self.peers.len();
        self.status.sinks = self.sinks.len();

        match self.room_tx.try_send(PeerRelayMessage::UpdateStatus(
            self.status.clone()
        )) {
            Err(TrySendError::Full(msg)) => {
                log::debug!("{:} channel is full! {:?}", self, msg);
            }
            //Ignore closed or OK
            _ => {}
        }
    }

    fn check_close_state(&mut self) {
        let close = match self.close_mode {
            PeerRelayCloseMode::NoPeers => self.peers.len() == 0,
            PeerRelayCloseMode::NoSinks => self.sinks.len() == 0,
        };
        if close && !self.is_closed() {
            log::debug!("{:} room is empty, closing", self);
            self.close();
        }
    }
    
    async fn ping_peers(&mut self) {
        for conn in self.peers.values() {
            if !conn.stream().is_last_contact_more_than(self.config.ping_wait_time) {
                continue;
            }
            if let Err(err) = conn.sink().send_relay_message(NetRelayMessageRelay::Ping, false).await {
                log::error!("{:} ping message to {:} error: {:}", self, conn, err);
            }
        }
    }

    ///Prune closed connections
    async fn prune_connections(&mut self) {
        let closed_netids = self.peers.iter()
            .filter_map(|(k, v)| {
                if v.is_closed() { Some(k.to_owned()) } else { None }
            })
            .collect::<Vec<_>>();
        for netid in closed_netids {
            self.handle_remove_connection(netid).await;
        }
    }
    
    ///Prune closed sinks
    async fn prune_sinks(&mut self) {
        let closed_netids = self.sinks.iter()
            .filter_map(|(k, v)| {
                if v.is_closed() { Some(k.to_owned()) } else { None }
            })
            .collect::<Vec<_>>();
        for netid in closed_netids {
            self.handle_remove_sink(netid).await;
        }
    }

    async fn poll_peers(&mut self) {
        let self_str = self.to_string();
        
        //Prune peers
        self.prune_connections().await;
        
        //Pick all messages that peers sent
        let mut relay_msgs = vec![];
        let mut peers_msgs = vec![];
        for conn in self.peers.values_mut() {
            let conn_netid = conn.get_netid();
            if conn_netid == NETID_NONE || conn_netid == NETID_ALL || conn_netid == NETID_RELAY {
                log::info!("{:} peer {:} has reserved NETID", self_str, conn);
                conn.close(936585274);
                continue
            }
            
            //Check timeout
            if conn.stream().has_contact_timeout() {
                log::info!("{:} peer {:} took too long to contact", self_str, conn);
                conn.close(917347483);
                continue
            }

            //Poll peers sent messages
            for msg in {
                match conn.stream_mut().try_read_messages(self.config.msgs_per_poll) {
                    NetConnectionRead::Empty => {
                        Vec::new()
                    },
                    NetConnectionRead::Data(msgs) => msgs,
                    NetConnectionRead::Closed => {
                        continue
                    }
                }.into_iter()
            } {
                if msg.source_netid != conn_netid {
                    log::debug!("{:} peer {:} sent message from different source NETID: {:}", self_str, conn, msg);
                    conn.close(98364723);
                } else if msg.destination_netid == conn_netid {
                    log::debug!("{:} peer {:} sent message to itself: {:}", self_str, conn, msg);
                    conn.close(98748384);
                } else if msg.destination_netid == NETID_NONE {
                    log::debug!("{:} peer {:} sent message to NETID_NONE: {:}", self_str, conn, msg);
                    conn.close(98275737);
                } else if msg.destination_netid == NETID_RELAY {
                    relay_msgs.push(msg);
                } else {
                    peers_msgs.push(msg);
                }
            }
        }

        //Prune sinks
        self.prune_sinks().await;

        //Process and send messages to corresponding destinations
        stream::iter(peers_msgs).for_each_concurrent(
            Some(self.config.msgs_per_poll),
            |msg| async {
                if msg.destination_netid == NETID_ALL {
                    //We need to send this message to all clients, so craft a message with each client destination
                    for sink in self.sinks.values() {
                        let destination_netid = sink.get_netid();
                        let client_msg = NetConnectionMessage {
                            source_netid: msg.source_netid,
                            destination_netid,
                            compressed: msg.compressed,
                            data: msg.data.clone(),
                        };
                        let info_task = self_str.clone();
                        let result = sink.send_message(client_msg, false).await;
                        if let Err(err) = result {
                            log::error!("{:} sending message to sink {:} error: {:}", info_task, sink, err);
                        }
                    }
                } else if let Some(sink) = self.sinks.get(&msg.destination_netid) {
                    if !sink.is_closed() {
                        if let Err(err) = sink.send_message(msg, false).await {
                            log::error!("{:} sending message to sink {:} error: {:}", self_str, sink, err);
                        }
                    }
                } else {
                    log::error!("{:} unknown destination for message: {:}", self_str, msg);
                }
            }
        ).await;
        
        //Process relay messages
        for msg in relay_msgs {
            self.handle_relay_message(msg).await;
        }

        //Run futures concurrently
        //while let Some(_) = futures.next().await {};
        
        //Flush sinks to send messages
        for sink in self.sinks.values() {
            if sink.is_closed() {
                continue;
            }
            if let Err(err) = sink.flush().await {
                log::trace!("{:} flushing client {:} error: {:}", self_str, sink, err);
            }
        }
    }
    
    async fn handle_relay_message(&mut self, msg: NetConnectionMessage) {
        let source_netid = msg.source_netid;
        let peer_msg = match NetRelayMessagePeer::try_from(msg) {
            Ok(peer_msg) => peer_msg,
            Err(err) => {
                log::error!("{:} peer {:} error decoding relay message: {:?}", self, source_netid, err);
                return;
            }
        };
        log::debug!("{:} handle_relay_message {:} msg {:?}", self, source_netid, peer_msg);

        match peer_msg {
            NetRelayMessagePeer::Close(code) => {
                if let Some(conn) = self.peers.get_mut(&source_netid) {
                    log::debug!("{:} got close message code {:}", conn, code);
                    conn.close(972534431);
                }
                self.handle_remove_sink(source_netid).await;
            }
            NetRelayMessagePeer::LeaveRoom => {
                if let Some(mut conn) = self.peers.remove(&source_netid) {
                    conn.set_netid(NETID_NONE);
                    if let Err(err) = self.room_tx.send(PeerRelayMessage::StoreConnection(conn)).await {
                        log::error!("{:} Couldn't send StoreConnection message for LeaveRoom! {:?}", self, err);
                    }
                }
                self.handle_remove_sink(source_netid).await;
            }
            NetRelayMessagePeer::SetupRoom { info, .. } => {
                if self.has_permission(source_netid) {
                    self.status.info = Some(info);
                }
            }
            NetRelayMessagePeer::PingResponse(ping) => {
                if let Some(conn) = self.peers.get_mut(&source_netid) {
                    conn.stream_mut().update_last_contact();
                    self.status.pings.insert(source_netid, ping.as_millis() as u32);
                }
            }
            NetRelayMessagePeer::ClosePeer(netid) => {
                if self.has_permission(source_netid) {
                    if let Some(conn) = self.peers.get_mut(&netid) {
                        conn.close(978367384);
                    };
                }
            }
            NetRelayMessagePeer::ListLobbyHosts { .. } |
            NetRelayMessagePeer::ListLobbies { .. } |
            NetRelayMessagePeer::JoinRoom(_) => {
                log::debug!("{:} unknown relay message: {:?}", self, peer_msg);
            }
        }
    }

    async fn process_queue(&mut self) {
        for _ in 0..self.config.channel_msgs_per_update {
            let msg = match self.room_rx.try_recv() {
                Ok(msg) => msg,
                Err(TryRecvError::Empty) => {
                    break;
                },
                Err(TryRecvError::Disconnected) => {
                    log::debug!("{:} room_rx disconnected", self);
                    self.close();
                    return;
                }
            };
    
            //Process it
            log::debug!("{:} processing room msg: {}", self, msg);
            match msg {
                PeerRelayMessage::StoreConnection(conn) => {
                    self.handle_store_connection(conn).await;
                }
                PeerRelayMessage::StoreSink(sink) => {
                    self.handle_store_sink(sink).await;
                }
                PeerRelayMessage::RemoveConnection(netid) => {
                    self.handle_remove_connection(netid).await;
                }
                PeerRelayMessage::RemoveSink(netid) => {
                    self.handle_remove_sink(netid).await;
                }
                PeerRelayMessage::SetOwnership(ownership) => {
                    self.ownership = ownership
                }
                PeerRelayMessage::UpdateStatus(_) => {
                    log::error!("{:} channel_from_room received unexpected message", self);
                }
            }
        }
    }
}

impl Display for PeerRelayStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "PeerRelayStatus {{ Peers/Sinks {}/{}, Pings: {:?}, Info: {:?} }}",
               self.peers, self.sinks, self.pings, self.info
        )
    }
}

impl Display for PeerRelayMessage {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl Display for PeerRelayCloseMode {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl Display for PeerRelayOwnership {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl Display for PeerRelay {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "PeerRelay {{ Room: {}, Peers/Sinks: {}/{}, Ownership: {}, Close: {} }}",
               self.config.room_id,
               self.peers.len(),
               self.sinks.len(),
               self.ownership,
               self.close_mode,
        )
    }
}

impl Drop for PeerRelay {
    fn drop(&mut self) {
        self.close();
    }
}
