use std::fmt::{Display, Formatter};
use std::time::Duration;
use tokio::join;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;
use tokio_util::sync::CancellationToken;
use crate::lobby::room_info::RoomInfo;
use crate::relay::peer_relay::{PeerRelay, PeerRelayCloseMode, PeerRelayConfig, PeerRelayMessage, PeerRelayOwnership};
use crate::netconnection::{NetConnection, NETID, NETID_CLIENTS_RELAY_START};
use crate::relay::session_manager::SessionManagerSender;
use crate::utils::config_from_str;

enum RoomTopology {
    PeerToPeer {
        ///Communication channel for relay
        relay_tx: mpsc::Sender<PeerRelayMessage>,
        ///Communication channel from relay
        relay_rx: mpsc::Receiver<PeerRelayMessage>,
    },
    ServerClient {
        ///Communication channel for server relay
        server_tx: mpsc::Sender<PeerRelayMessage>,
        ///Communication channel from server relay
        server_rx: mpsc::Receiver<PeerRelayMessage>,
        ///Communication channel for clients relay
        clients_tx: mpsc::Sender<PeerRelayMessage>,
        ///Communication channel from clients relay
        clients_rx: mpsc::Receiver<PeerRelayMessage>,
    },
}

pub struct Room {
    ///Contains room information that will be accessed by outside
    pub info: RoomInfo,
    ///Channel to session manager
    #[allow(dead_code)]
    session_manager: SessionManagerSender,
    ///Last reported number of peers in room by task
    peers: usize,
    ///Next NETID to give to a incoming client
    client_netid_next: NETID,
    ///Token to cancel task
    cancellation: CancellationToken,
    ///Max amount of messages to poll from channels on each update
    channel_msgs_per_update: usize,
    ///Topology of this room
    room_topology: RoomTopology,
}

impl Room {    
    pub fn new_server_client(conn: NetConnection,
                             info: RoomInfo,
                             session_manager: SessionManagerSender
    ) -> Self {
        let channel_room_to_task_size = config_from_str::<usize>("SERVER_ROOM_TASK_CHANNEL_TX_SIZE", 20);
        let channel_task_to_room_size = config_from_str::<usize>("SERVER_ROOM_TASK_CHANNEL_RX_SIZE", 10);
        let (room_to_server_tx, room_to_server_rx) = mpsc::channel(channel_room_to_task_size);
        let (room_to_clients_tx, room_to_clients_rx) = mpsc::channel(channel_room_to_task_size);
        let (server_to_room_tx, server_to_room_rx) = mpsc::channel(channel_task_to_room_size);
        let (clients_to_room_tx, clients_to_room_rx) = mpsc::channel(channel_task_to_room_size);
        let config_server = PeerRelayConfig::new(info.room_id);
        let cancellation = config_server.cancellation.clone();

        //Create relay for clients -> host
        let mut config_clients = config_server.clone();
        config_clients.status_timer.duration = Duration::ZERO; //No status report need
        PeerRelay::new(
            config_clients,
            clients_to_room_tx,
            room_to_clients_rx,
            PeerRelayCloseMode::NoSinks,
            PeerRelayOwnership::Peer(conn.get_netid()),
            vec![],
            vec![conn.clone_sink()]
        ).run();

        //Create relay for host -> clients
        PeerRelay::new(
            config_server,
            server_to_room_tx,
            room_to_server_rx,
            PeerRelayCloseMode::NoPeers,
            PeerRelayOwnership::Peer(conn.get_netid()),
            vec![conn],
            vec![]
        ).run();

        Self {
            info,
            session_manager,
            cancellation,
            channel_msgs_per_update: config_from_str::<usize>("SERVER_ROOM_TASK_CHANNEL_MSGS_PER_UPDATE", 10),
            peers: 1,
            client_netid_next: NETID_CLIENTS_RELAY_START,
            room_topology: RoomTopology::ServerClient {
                server_tx: room_to_server_tx,
                server_rx: server_to_room_rx,
                clients_tx: room_to_clients_tx,
                clients_rx: clients_to_room_rx,
            },
        }
    }
    
    pub fn new_peer_to_peer(conn: NetConnection,
                            info: RoomInfo,
                            session_manager: SessionManagerSender
    ) -> Self {
        let channel_room_to_task_size = config_from_str::<usize>("SERVER_ROOM_TASK_CHANNEL_TX_SIZE", 20);
        let channel_task_to_room_size = config_from_str::<usize>("SERVER_ROOM_TASK_CHANNEL_RX_SIZE", 10);
        let (relay_tx, room_rx) = mpsc::channel(channel_room_to_task_size);
        let (room_tx, relay_rx) = mpsc::channel(channel_task_to_room_size);
        let config = PeerRelayConfig::new(info.room_id);
        let cancellation = config.cancellation.clone();

        //Create relay for all peers
        let conn_sink = conn.clone_sink();
        PeerRelay::new(
            config,
            room_tx,
            room_rx,
            PeerRelayCloseMode::NoPeers,
            PeerRelayOwnership::None,
            vec![conn],
            vec![conn_sink],
        ).run();
        
        Self {
            info,
            session_manager,
            cancellation,
            channel_msgs_per_update: config_from_str::<usize>("SERVER_ROOM_TASK_CHANNEL_MSGS_PER_UPDATE", 10),
            peers: 1,
            client_netid_next: NETID_CLIENTS_RELAY_START,
            room_topology: RoomTopology::PeerToPeer {
                relay_tx,
                relay_rx,
            },
        }
    }

    pub fn close(&mut self) {
        if !self.cancellation.is_cancelled() {
            log::info!("Room close {}", self);
        }
        self.cancellation.cancel();
        match &mut self.room_topology {
            RoomTopology::PeerToPeer { relay_rx, .. } => {
                relay_rx.close();
            }
            RoomTopology::ServerClient { server_rx, clients_rx, .. } => {
                server_rx.close();
                clients_rx.close();
            }
        }
    }

    pub async fn add_connection(&mut self, mut conn: NetConnection) {
        if self.cancellation.is_cancelled() {
            return;
        }

        //Assign NETID to connection
        let netid = self.client_netid_next;
        self.client_netid_next += 1;
        conn.set_netid(netid);

        //Handle per topology
        let (sink_tx, conn_tx) = match &self.room_topology {
            RoomTopology::PeerToPeer { relay_tx, .. } => {
                (relay_tx, relay_tx)
            }
            RoomTopology::ServerClient { server_tx, clients_tx, .. } => {
                (server_tx, clients_tx)
            }
        };
        let sink = conn.clone_sink();
        let result = join!(
            sink_tx.send(PeerRelayMessage::StoreSink(sink)),
            conn_tx.send(PeerRelayMessage::StoreConnection(conn)),
        );
        if result.0.is_err() || result.1.is_err() {
            log::error!("{} Couldn't send sink or connection message!", self);
        }
    }

    pub fn is_active(&self) -> bool {
        !self.cancellation.is_cancelled() && 0 < self.peers
    }

    ///Updates state of room
    pub async fn update(&mut self) {
        if self.cancellation.is_cancelled() {
            return;
        }

        let mut messages = vec![];
        for _ in 0..self.channel_msgs_per_update {
            if messages.len() > self.channel_msgs_per_update {
                break;
            }
            match &mut self.room_topology {
                RoomTopology::PeerToPeer { relay_rx, .. } => {
                    match relay_rx.try_recv() {
                        Ok(msg) => messages.push(msg),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            log::error!("{} relay rx disconnected", self);
                            self.close();
                            return;
                        },
                    }
                },
                RoomTopology::ServerClient { server_rx, clients_rx, .. } => {
                    let mut server_empty = false;
                    let mut clients_empty = false;
                    match server_rx.try_recv() {
                        Ok(msg) => messages.push(msg),
                        Err(TryRecvError::Empty) => server_empty = true,
                        Err(TryRecvError::Disconnected) => {
                            log::error!("{} server rx disconnected", self);
                            self.close();
                            return;
                        },
                    }
                    match clients_rx.try_recv() {
                        Ok(msg) => messages.push(msg),
                        Err(TryRecvError::Empty) => clients_empty = true,
                        Err(TryRecvError::Disconnected) => {
                            log::error!("{} clients rx disconnected", self);
                            self.close();
                            return;
                        },
                    }
                    if server_empty && clients_empty {
                        break;
                    }
                },
            };
        }

        //Process it
        for msg in messages {
            log::trace!("{:} processing room relay msg: {}", self, msg);
            match msg {
                PeerRelayMessage::UpdateStatus(status) => {
                    //Update room info from status
                    self.info.ping = 0;
                    for ping in status.pings.values() {
                        self.info.ping = self.info.ping.max(*ping);
                    }
                    if let RoomTopology::ServerClient { .. } = self.room_topology {
                        self.peers = status.peers + status.sinks;
                    } else {
                        self.peers = status.peers;
                    }
                    if let Some(info) = status.info {
                        self.info.update(info);
                    }
                },
                PeerRelayMessage::StoreConnection(conn) => {
                    //Remove any sink that rooms may have
                    //Peer to peer doesn't need this since all is on same relay which already does internal cleanup
                    if let RoomTopology::ServerClient { server_tx, clients_tx, .. } = &self.room_topology {
                        let result = join!(
                            server_tx.send(PeerRelayMessage::RemoveSink(conn.get_netid())),
                            clients_tx.send(PeerRelayMessage::RemoveSink(conn.get_netid())),
                        );
                        if result.0.is_err() || result.1.is_err() {
                            log::error!("{} Couldn't send sink message!", self);
                        }
                    };

                    //Send to session manager
                    self.session_manager.incoming_connection(conn).await;
                },
                PeerRelayMessage::RemoveConnection(conn) => {
                    //Remove any connections that rooms may have, this is usually sent to Room when a peer receives ClosePeer
                    //Peer to peer doesn't need this since all is on same relay which already does internal cleanup
                    if let RoomTopology::ServerClient { server_tx, clients_tx, .. } = &self.room_topology {
                        let result = join!(
                            server_tx.send(PeerRelayMessage::RemoveConnection(conn)),
                            clients_tx.send(PeerRelayMessage::RemoveConnection(conn)),
                        );
                        if result.0.is_err() || result.1.is_err() {
                            log::error!("{} Couldn't send sink message!", self);
                        }
                    };
                },
                PeerRelayMessage::StoreSink(_) |
                PeerRelayMessage::RemoveSink(_) |
                PeerRelayMessage::SetOwnership(_) => {
                    log::error!("{} received unexpected msg from relays: {}", self, msg);
                }
            }
        }
    }
}

impl Display for Room {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "Room {{ Info: {}, Peers: {} }}",
               self.info,
               self.peers,
        )
    }
}

impl Drop for Room {
    fn drop(&mut self) {
        self.close();
    }
}

