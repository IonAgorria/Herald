use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use serde::{Deserialize, Serialize};
use validator::{Validate, ValidationError};

pub type RoomID = u64;

#[derive(Debug, Clone, Serialize)]
pub struct LobbyHost {
    pub host_tcp: String,
    pub host_ws: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LobbyWithRooms {
    pub host: LobbyHost,
    pub rooms: Vec<RoomInfo>,
}

pub fn validator_room_extra_data(extra_datas: &HashMap<String, String>) -> Result<(), ValidationError> {
    if 64 < extra_datas.len() {
        return Err(ValidationError {
            code: "extra_data".into(),
            message: Some("Too many extra_data".into()),
            params: Default::default(),
        });
    }
    for pair in extra_datas {
        if 64 < pair.0.len() {
            return Err(ValidationError {
                code: "extra_data".into(),
                message: Some(format!("extra_data[{}] = {} key is too long", pair.0, pair.1).into()),
                params: Default::default(),
            });
        }
        if 128 < pair.1.len() {
            return Err(ValidationError {
                code: "extra_data".into(),
                message: Some(format!("extra_data[{}] = {} value is too long", pair.0, pair.1).into()),
                params: Default::default(),
            });
        }
    }
    Ok(())
}

/*
fn validate_room(room: &RoomInfo) -> Result<(), ValidationError> {
    Ok(())
}
*/

#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
//#[validate(schema(function = "validate_room", skip_on_field_errors = true))]
pub struct RoomInfo {
    ///Room ID
    pub room_id: RoomID,
    ///Game identifier for this room 
    #[validate(length(min = 5, max = 32))]
    pub game_type: String,
    ///Game version required for this room
    #[validate(length(min = 3, max = 32))]
    pub game_version: String,
    ///Public name of room to display in list
    #[validate(length(min = 1, max = 64))]
    pub room_name: String,
    ///Is this room password protected?
    pub room_password: bool,
    ///Is this room closed?
    pub room_closed: bool,
    ///Is this room game started?
    pub game_started: bool,
    ///Current amount of players this room has
    #[validate(range(min = 0, max = 999))]
    pub players_count: u16,
    ///Max amount of players this room can hold
    #[validate(range(min = 0, max = 999))]
    pub players_max: u16,
    ///Ping in millis between game host and relay or highest among peers
    #[validate(range(min = 0, max = 99999))]
    pub ping: u32,
    ///Game specific data for this room
    #[validate(custom(function = "validator_room_extra_data"))]
    pub extra_data: HashMap<String, String>,
}

impl RoomInfo {
    pub fn update(&mut self, info: RoomInfo) {
        self.game_type = info.game_type;
        self.game_version = info.game_version;
        self.room_name = info.room_name;
        self.room_password = info.room_password;
        self.room_closed = info.room_closed;
        self.game_started = info.game_started;
        self.players_count = info.players_count;
        self.players_max = info.players_max;
        self.extra_data = info.extra_data;
    }
}

impl Display for RoomInfo {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "RoomInfo {{ id: {}, name: {}, players: {}/{}, PW: {}, ping: {}, game: {} {} }}", 
                self.room_id,
                self.room_name,
                self.players_count,
                self.players_max,
                self.room_password,
                self.ping,
                self.game_type,
                self.game_version,
        )
    }
}
