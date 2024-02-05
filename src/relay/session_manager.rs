use Default;
use std::sync::Arc;
use tokio::sync::mpsc;
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc::error::SendError;
use tokio::task::JoinHandle;
use tokio::time::interval;
use crate::AppData;
use crate::lobby::lobby_manager::LobbyManager;
use crate::lobby::room_info::LobbyWithRooms;
use crate::relay::messages::{NetRelayMessagePeer, NetRelayMessageRelay};
use crate::relay::netconnection::{NETID_NONE, NetConnection, NetConnectionRead};
use crate::utils::config_from_str;

enum SessionManagerMessage {
    Ping(SystemTime),
    StoreConnection(NetConnection),
    TransferConnection(NetConnection, NetRelayMessagePeer),
    ReplyConnection(NetConnection, NetRelayMessagePeer),
}

#[derive(Clone)]
pub struct SessionManagerSender(mpsc::Sender<SessionManagerMessage>);

///Handles the initial handshake and other game-relay interactions
///such as creating or joining rooms, etc
pub struct SessionManager {
    pub task: JoinHandle<()>,
    pub queue_tx: SessionManagerSender,
}

struct SessionManagerState {
    running: bool,
    poll_interval: u64,
    peer_timeout: u64,
    queue_tx: SessionManagerSender,
    queue_rx: mpsc::Receiver<SessionManagerMessage>,
    peers: Box<Vec<NetConnection>>,
    lobby_manager: LobbyManager,
    app_data: Arc<AppData>,
}

impl SessionManager {
    pub fn init(app_data: Arc<AppData>) -> Self {
        let poll_interval = config_from_str::<u64>("SERVER_SESSION_MANAGER_POLL_INTERVAL", 10);
        let peer_timeout = config_from_str::<u64>("SERVER_SESSION_MANAGER_PEER_TIMEOUT", 5000);
        let channel_size = config_from_str::<usize>("SERVER_SESSION_MANAGER_MESSAGES_MAX", 1000);
        let (queue_tx, queue_rx) = mpsc::channel(channel_size);
        let session_manager_sender = SessionManagerSender(queue_tx.clone());
        let state = SessionManagerState {
            running: true,
            poll_interval,
            peer_timeout,
            queue_tx: session_manager_sender.clone(),
            queue_rx,
            peers: Default::default(),
            lobby_manager: LobbyManager::new(app_data.clone(), session_manager_sender),
            app_data,
        };
        
        // Spawn thread and return struct
        let task = tokio::spawn(state.entry());
        
        Self {
            queue_tx: SessionManagerSender(queue_tx),
            task,
        }
    }
}

impl SessionManagerSender {
    async fn send(&self, msg: SessionManagerMessage) -> Result<(), SendError<SessionManagerMessage>> {
        self.0.send(msg).await
    }
    
    async fn send_log(&self, msg: SessionManagerMessage) -> bool {
        if let Err(err) = self.send(msg).await {
            log::error!("Couldn't send internal message! {:?}", err);
            false
        } else {
            true
        }
    }

    pub async fn ping(&self) -> bool {
        self.send_log(SessionManagerMessage::Ping(SystemTime::now())).await
    }

    pub async fn incoming_connection(&self, conn: NetConnection) -> bool {
        self.send_log(SessionManagerMessage::StoreConnection(conn)).await
    }
}

impl SessionManagerState {
    async fn entry(mut self) {
        let mut interval = interval(Duration::from_millis(self.poll_interval));
        while self.running {
            self.lobby_manager.update().await;
            self.poll_peers().await;
            self.process_queue().await;
            
            //Eepy time, also important to let this loop don't block the async
            interval.tick().await;
        }
    }
    
    async fn process_queue(&mut self) {
        //Try fetching from queue
        let msg = match self.queue_rx.try_recv() {
            Ok(msg) => msg,
            Err(mpsc::error::TryRecvError::Empty) => { 
                return;
            },
            Err(mpsc::error::TryRecvError::Disconnected) => {
                log::error!("SessionManager queue disconnected");
                self.running = false;
                return;
            }
        };

        //Process it
        match msg {
            SessionManagerMessage::Ping(past) => {
                log::info!(
                    "SessionManagerMessage::Ping {}",
                    SystemTime::now()
                        .duration_since(past)
                        .map(|d| d.as_micros())
                        .unwrap_or(0)
                    );
            }
            SessionManagerMessage::StoreConnection(mut conn) => {
                //Accept session for polling
                if conn.get_netid() != NETID_NONE {
                    conn.set_netid(NETID_NONE);
                }
                self.peers.push(conn);
            }
            SessionManagerMessage::TransferConnection(mut conn, msg) => {
                match msg {
                    NetRelayMessagePeer::SetupRoom { info, topology } => {
                        self.lobby_manager.connection_create_room(conn, info, topology).await;
                    },
                    NetRelayMessagePeer::JoinRoom(info) => {
                        self.lobby_manager.connection_join_room(conn, info).await;
                    },
                    msg => {
                        log::error!("TransferConnection unknown message {:?}", msg);
                        conn.close(175738375);
                    }
                }
            },
            SessionManagerMessage::ReplyConnection(conn, msg) => {
                tokio::spawn(Self::process_connection_reply(
                    conn, msg, self.queue_tx.clone(), self.app_data.clone()
                ));
            }
        }
    }

    async fn poll_peers(&mut self) {
        //We take all peers currently available and send them to tasks for processing, then collect back
        let peers = std::mem::replace(&mut self.peers, Box::new(Vec::new()));

        for conn in peers.into_iter() {
            let queue_tx = self.queue_tx.clone();
            tokio::spawn(Self::process_peer_connection(self.peer_timeout, conn, queue_tx));
        }
    }

    async fn process_peer_connection(peer_timeout: u64, mut conn: NetConnection, queue_tx: SessionManagerSender) {
        let message = match conn.stream_mut().read_message().await {
            NetConnectionRead::Closed => {
                //Nothing to do here since is closed
                return;
            },
            NetConnectionRead::Empty => {
                //No message to read, send connection back
                if let Err(err) = queue_tx.send(
                    SessionManagerMessage::StoreConnection(conn)
                ).await {
                    log::error!("Couldn't send empty connection to queue! {:?}", err);
                }
                return;
            },
            NetConnectionRead::Data(msg) => {
                match NetRelayMessagePeer::try_from(msg) {
                    Ok(data) => data,
                    Err(err) => {
                        log::error!("Peer {:} error when parsing a message: {:?}", conn, err);
                        conn.close(1948466945);
                        return;
                    }
                }
            },
        };
        
        //Check if peer took too much time to answer
        if conn.is_last_contact_more_than(peer_timeout) {
            //We just close and drop it here, no point going further to be discarded anyway
            log::info!("Peer {:} took too long to contact", conn);
            return; //Conn dropped here
        }

        let queue_message = match message {
            NetRelayMessagePeer::Close(code) => {
                log::debug!("Connection {:} got close message code {:}", conn, code);
                return; //Conn dropped here
            }
            NetRelayMessagePeer::ListLobbies {..} => {
                //Reply with the rooms, we need access to rooms list
                SessionManagerMessage::ReplyConnection(conn, message)
            }
            NetRelayMessagePeer::SetupRoom {..} |
            NetRelayMessagePeer::JoinRoom(..) => {
                //Needs to be transferred to a room, but we can't do that inside this task
                SessionManagerMessage::TransferConnection(conn, message)
            },
            _ => {
                log::error!("Connection {:} unexpected message type: {:?}", conn, message);
                return; //Conn dropped here
            }
        };

        if let Err(err) = queue_tx.send(queue_message).await {
            log::error!("Couldn't send processed connection to queue! {:?}", err);
        }
    }
    
    async fn process_connection_reply(mut conn: NetConnection, msg_peer: NetRelayMessagePeer,
                                      queue_tx: SessionManagerSender, app_data: Arc<AppData>) {
        //First assemble the relay reply
        let msg = match msg_peer {
            NetRelayMessagePeer::ListLobbies { game_type, format } => {
                let room_infos_guard = app_data.room_infos.load();
                let lobbies = vec![
                    LobbyWithRooms {
                        host: app_data.tcp_public_address.clone(),
                        rooms: LobbyManager::filter_room_by_game_type(&room_infos_guard, &game_type),
                    }
                ];
                NetRelayMessageRelay::ListLobbies {
                    lobbies, 
                    format,
                }
            },
            msg => {
                log::error!("ReplyConnection unknown message {:?}", msg);
                conn.close(1593483496);
                return;
            }
        };

        //Send and flush, then store connection back
        if let Err(err) = conn.sink().send_relay_message(msg, true).await {
            log::error!("Connection {:} error sending reply {:?}", conn, err);
            return; //Conn dropped here
        }

        conn.update_last_contact();
        if let Err(err) = queue_tx.send(
            SessionManagerMessage::StoreConnection(conn)
        ).await {
            log::error!("Couldn't send processed connection to queue! {:?}", err);
        }
    }
}