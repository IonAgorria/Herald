use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering::Relaxed;
use arc_swap::ArcSwap;
use crate::AppData;
use crate::lobby::room_info::{RoomInfo, RoomID};
use crate::lobby::room::Room;
use crate::netconnection::{NetConnection, NETID_HOST};

type RoomMap = HashMap<RoomID, Room>;
pub type CurrentRoomInfos = ArcSwap<Vec<RoomInfo>>;

///Contains basic info required to join a room
#[derive(Debug)]
pub struct RoomJoinInfo {
    pub game_type: String,
    pub game_version: String,
    pub room_id: RoomID
}

impl Display for RoomJoinInfo {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "RoomJoinInfo {{ Id: {} Game: {} {} }}",
               self.room_id,
               self.game_type,
               self.game_version
        )
    }
}

pub struct LobbyManager {
    ///Unique identifier for rooms
    room_next_id: AtomicU64,
    ///Internal map of rooms, do not use any await while holding a guard!
    rooms: RoomMap,
    ///App data
    app_data: Arc<AppData>,
}

impl LobbyManager {
    pub fn new(app_data: Arc<AppData>) -> Self {
        Self {
            room_next_id: AtomicU64::new(1),
            rooms: Default::default(),
            app_data,
        }
    }
    
    fn get_next_room_id(&self) -> RoomID {
        self.room_next_id.fetch_add(1, Relaxed)
    }
    
    fn refresh_room_infos(&mut self) {
        let mut room_infos = Vec::new();
        for room in self.rooms.values() {
            room_infos.push(room.info.clone());
        }
        self.app_data.room_infos.store(Arc::new(room_infos));
    }
    
    pub fn filter_room_by_game_type(room_infos: &Arc<Vec<RoomInfo>>, game_type: &String) -> Vec<RoomInfo> {
        if game_type.is_empty() {
            room_infos.as_ref().to_owned()
        } else {
            room_infos
                .iter()
                .filter_map(|r| {
                    if r.game_type.eq(game_type) {
                        Some(r.to_owned())
                    } else {
                        None
                    }
                })
                .collect()
        }
    }
    
    fn connection_close(mut conn: NetConnection, code: u32) {
        log::debug!("connection_close {:} code: {:}", conn, code);
        tokio::spawn(async move {
            conn.close(code);
            //Conn dropped here
        });
    }

    pub async fn connection_create_room(&mut self, mut conn: NetConnection, mut info: RoomInfo, topology: u16) {
        log::debug!("connection_create_room {:} info: {:} topology: {:}", conn, info, topology);
        if info.room_id != 0 {
            Self::connection_close(conn, 2);
            return;
        }
        if topology == 0 || 2 < topology {
            Self::connection_close(conn, 3);
            return;
        }


        //Setup some stuff
        let room_id = self.get_next_room_id();
        info.room_id = room_id;
        conn.set_netid(NETID_HOST); //The connection who created the room has NETID HOST assigned
        log::info!("{:} creating new room {:}", conn, info);

        //Create and insert room
        let session_manager = self.app_data.session_manager.clone();
        let room = match topology {
            1 => Room::new_server_client(conn, info, session_manager),
            2 => Room::new_peer_to_peer(conn, info, session_manager),
            _ => {
                //Not supposed to get here since we check topology before
                log::error!("Unknown topology {:} for room {:}", topology, info);
                return;
            }
        };
        if let Some(old) = self.rooms.insert(room_id, room) {
            log::error!("Replaced existing room {:}", old);
        }
    }

    pub async fn connection_join_room(&mut self, conn: NetConnection, info: RoomJoinInfo) {
        log::debug!("connection_join_room {:} info: {:}", conn, info);
        
        //Try to retrieve room that wants to be joined
        let room = match self.rooms.get(&info.room_id) {
            Some(room) => room,
            None => {
                log::info!("{:} attempted to join a unknown room, info: {:}", conn, info);
                Self::connection_close(conn, 2);
                return;
            }
        };

        //Check if room can be joined
        assert_eq!(room.info.room_id, info.room_id); //ID mismatch not supposed to happen
       if room.info.room_id != info.room_id
       || room.info.game_type != info.game_type
       || room.info.game_version != info.game_version {
           log::info!("{:} attempted to join a incompatible room, info: {:} room: {:}", conn, info, room);
            Self::connection_close(conn, 3);
            return;
        }
        if room.info.room_closed {
            log::info!("{:} attempted to join a closed room, info: {:} room: {:}", conn, info, room);
            Self::connection_close(conn, 4);
            return;
        }

        //Add to room
        if let Some(room) = self.rooms.get_mut(&info.room_id) {
            log::info!("{:} joining a room, info: {:} room: {:}", conn, info, room);
            room.add_connection(conn).await;
        } else {
            log::error!("getting mutable room error when joining, conn {:} info: {:}", conn, info);
        }
    }
    
    pub async fn update(&mut self) {
        self.refresh_room_infos();
        for room in self.rooms.values_mut() {
            room.update().await;
        }
        self.rooms.retain(|_, room| room.is_active());
    }
}