use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::io;
use std::time::Duration;
use actix_web::web::BytesMut;
use num_derive::FromPrimitive;
use num_traits::FromPrimitive;
use tokio_util::bytes::{Buf, BufMut, Bytes};
use serde::{Serialize};
use crate::lobby::lobby_manager::RoomJoinInfo;
use crate::lobby::room_info::{LobbyHost, LobbyWithRooms, RoomID, RoomInfo};
use crate::netconnection::{NETID, NETID_ALL, NETID_HOST, NETID_NONE, NETID_RELAY};
use crate::netconnection::NetConnectionMessage;
use crate::utils::time::{duration_since_timestamp, Timestamp};
use crate::utils::xprm::serialize_xprm_or_json;

const NET_RELAY_PROTOCOL_VERSION: u8 = 1;
const NET_RELAY_MAX_STRING_LENGTH: u16 = 256;
const NET_RELAY_MAX_MAP_ELEMENTS: u16 = 64;

#[derive(FromPrimitive, Debug)]
#[allow(non_camel_case_types)]
#[repr(u32)]
pub enum NetRelayMessageType {
    //Common
    RELAY_MSG_UNKNOWN = 0,
    RELAY_MSG_CLOSE,

    //Sent by peer to relay
    RELAY_MSG_PEER_START = 0x10000,
    RELAY_MSG_PEER_LIST_LOBBIES,
    RELAY_MSG_PEER_SETUP_ROOM,
    RELAY_MSG_PEER_JOIN_ROOM,
    RELAY_MSG_PEER_LEAVE_ROOM,
    RELAY_MSG_PEER_PING_RESPONSE,
    RELAY_MSG_PEER_CLOSE_PEER,
    RELAY_MSG_PEER_LIST_LOBBY_HOSTS,
    RELAY_MSG_PEER_BRIDGE_ROOM,

    //Sent by relay to peer
    RELAY_MSG_RELAY_START = 0x20000,
    RELAY_MSG_RELAY_LIST_LOBBIES,
    RELAY_MSG_RELAY_PING,
    RELAY_MSG_RELAY_LIST_PEERS,
    RELAY_MSG_RELAY_ADD_PEER,
    RELAY_MSG_RELAY_REMOVE_PEER,
    RELAY_MSG_RELAY_LIST_LOBBY_HOSTS,
}

#[derive(Debug, Serialize)]
pub enum NetRelayMessageError {
    WrongSourceNETID(NETID),
    WrongDestinationNETID(NETID),
    WrongProtocol(u8),
    UnsupportedFlag(u16),
    UnknownType(u32),
    MissingData,
    TooManyElements(u16),
    TooLongString(u16),
    NotUTF8String(usize),
}

#[derive(Debug)]
pub enum NetRelayMessage {
    Close(u32),
    /// Sent by peer
    PeerPingResponse(Duration),
    PeerListLobbyHosts {
        game_type: String,
        game_version: String,
        format: u16,
    },
    PeerListLobbies {
        game_type: String,
        game_version: String,
        format: u16,
    },
    PeerSetupRoom {
        info: RoomInfo,
        topology: u16,
    },
    PeerJoinRoom(RoomJoinInfo),
    PeerLeaveRoom,
    PeerClosePeer(NETID),
    PeerBridgeRoom {
        room_id: RoomID,
        token: String,
    },
    /// Sent by relay
    RelayPing(Timestamp),
    RelayListLobbyHosts {
        hosts: Vec<LobbyHost>,
        format: u16
    },
    RelayListLobbies {
        lobbies: Vec<LobbyWithRooms>,
        format: u16
    },
    RelayListPeers(Vec<NETID>),
    RelayAddPeer(NETID),
    RelayRemovePeer(NETID),
}

impl NetRelayMessage {
    pub fn read_string(data: &mut Bytes, max_len: u16) -> Result<String, NetRelayMessageError> {
        let len = data.get_u16_le();
        if len > max_len.min(NET_RELAY_MAX_STRING_LENGTH) {
            return Err(NetRelayMessageError::TooLongString(len));
        }
        if len as usize > data.len() {
            return Err(NetRelayMessageError::MissingData);
        }
        let buf = data.split_to(len as usize);
        match std::str::from_utf8(buf.as_ref()) {
            Ok(str) => Ok(String::from(str)),
            Err(err) => Err(NetRelayMessageError::NotUTF8String(err.valid_up_to())),
        }
    }
    
    pub fn read_map(data: &mut Bytes, max_elements: u16, max_key_len: u16, max_value_len: u16) -> Result<HashMap<String, String>, NetRelayMessageError> {
        let len = data.get_u16_le();
        if len > max_elements.min(NET_RELAY_MAX_MAP_ELEMENTS) {
            return Err(NetRelayMessageError::TooManyElements(len));
        }
        let mut map = HashMap::new();
        for _ in 0..len {
            let key = Self::read_string(data, max_key_len)?;
            let value = Self::read_string(data, max_value_len)?;
            map.insert(key, value);
        }
        Ok(map)
    }
    
    pub fn from_message(mut msg: NetConnectionMessage) -> Result<Self, NetRelayMessageError> {
        if msg.compressed {
            //Shouldn't be compressed
            return Err(NetRelayMessageError::UnsupportedFlag(NetConnectionMessage::FLAG_COMPRESSED));
        } else if msg.destination_netid != NETID_RELAY {
            return Err(NetRelayMessageError::WrongDestinationNETID(msg.destination_netid));
        }
        
        //Parse header
        let data = &mut msg.data;
        let head = data.get_u32_le();
        let relay_protocol: u8 = ((head >> 24) & 0xFF) as u8;
        if relay_protocol != NET_RELAY_PROTOCOL_VERSION {
            return Err(NetRelayMessageError::WrongProtocol(relay_protocol));
        }
        let msg_type = NetRelayMessageType::from_u32(head & 0xFF_FFFF)
            .unwrap_or(NetRelayMessageType::RELAY_MSG_UNKNOWN);

        //Check source NETID
        if msg.source_netid == NETID_RELAY || msg.source_netid == NETID_ALL {
            return Err(NetRelayMessageError::WrongSourceNETID(msg.source_netid));
        }
        match msg_type {
            NetRelayMessageType::RELAY_MSG_PEER_SETUP_ROOM => {
                if msg.source_netid != NETID_HOST {
                    return Err(NetRelayMessageError::WrongSourceNETID(msg.source_netid));
                }
            },
            NetRelayMessageType::RELAY_MSG_PEER_JOIN_ROOM => {
                if msg.source_netid != NETID_NONE {
                    return Err(NetRelayMessageError::WrongSourceNETID(msg.source_netid));
                }
            },
            _ => {}
        };
        let result = match msg_type {
            NetRelayMessageType::RELAY_MSG_CLOSE => {
                let code  = data.get_u32_le();
                NetRelayMessage::Close(code)
            },
            NetRelayMessageType::RELAY_MSG_PEER_LEAVE_ROOM => NetRelayMessage::PeerLeaveRoom,
            NetRelayMessageType::RELAY_MSG_PEER_LIST_LOBBY_HOSTS => {
                let game_type = Self::read_string(data, 32)?;
                let game_version = Self::read_string(data, 32)?;
                let format = data.get_u16_le();
                NetRelayMessage::PeerListLobbyHosts {
                    game_type,
                    game_version,
                    format
                }
            },
            NetRelayMessageType::RELAY_MSG_PEER_LIST_LOBBIES => {
                let game_type = Self::read_string(data, 32)?;
                let game_version = Self::read_string(data, 32)?;
                let format = data.get_u16_le();
                NetRelayMessage::PeerListLobbies {
                    game_type,
                    game_version,
                    format
                }
            },
            NetRelayMessageType::RELAY_MSG_PEER_SETUP_ROOM => {
                let game_type = Self::read_string(data, 32)?;
                let game_version = Self::read_string(data, 32)?;
                let room_name = Self::read_string(data, 64)?;
                let room_password = data.get_u8() != 0;
                let room_closed = data.get_u8() != 0;
                let game_started = data.get_u8() != 0;
                let players_count = data.get_u16_le();
                let players_max = data.get_u16_le();
                let topology = data.get_u16_le();
                let extra_data = Self::read_map(data, 32, 64, 128)?;
                NetRelayMessage::PeerSetupRoom {
                    info: RoomInfo {
                        room_id: 0,
                        room_created_at: 0,
                        room_bridged: Default::default(),
                        game_type,
                        game_version,
                        room_name,
                        room_password,
                        room_closed,
                        game_started,
                        players_count,
                        players_max,
                        ping: 0,
                        extra_data,
                    },
                    topology,
                }
            },
            NetRelayMessageType::RELAY_MSG_PEER_JOIN_ROOM => {
                let game_type = Self::read_string(data, 32)?;
                let game_version = Self::read_string(data, 32)?;
                let room_id = data.get_u64_le();
                NetRelayMessage::PeerJoinRoom(RoomJoinInfo {
                    game_type,
                    game_version,
                    room_id,
                })
            },
            NetRelayMessageType::RELAY_MSG_PEER_BRIDGE_ROOM => {
                let room_id = data.get_u64_le();
                let token = Self::read_string(data, 32)?;
                NetRelayMessage::PeerBridgeRoom {
                    room_id,
                    token
                }
            },
            NetRelayMessageType::RELAY_MSG_PEER_PING_RESPONSE => {
                let stamp_secs = data.get_u64_le();
                let stamp_subsecs = data.get_u32_le();
                let latency = duration_since_timestamp(&(stamp_secs, stamp_subsecs));
                NetRelayMessage::PeerPingResponse(latency)
            },
            NetRelayMessageType::RELAY_MSG_PEER_CLOSE_PEER => {
                let netid = data.get_u64_le();
                NetRelayMessage::PeerClosePeer(netid)
            },
            NetRelayMessageType::RELAY_MSG_RELAY_PING => {
                let stamp_secs = data.get_u64_le();
                let stamp_subsecs = data.get_u32_le();
                NetRelayMessage::RelayPing((stamp_secs, stamp_subsecs))
            },
            NetRelayMessageType::RELAY_MSG_RELAY_ADD_PEER => {
                let netid  = data.get_u64_le();
                NetRelayMessage::RelayAddPeer(netid)
            },
            NetRelayMessageType::RELAY_MSG_RELAY_REMOVE_PEER => {
                let netid  = data.get_u64_le();
                NetRelayMessage::RelayRemovePeer(netid)
            },
            //Unknown and relay side messages go here   
            NetRelayMessageType::RELAY_MSG_PEER_START |
            NetRelayMessageType::RELAY_MSG_RELAY_START |
            NetRelayMessageType::RELAY_MSG_RELAY_LIST_LOBBIES |
            NetRelayMessageType::RELAY_MSG_RELAY_LIST_PEERS |
            NetRelayMessageType::RELAY_MSG_RELAY_LIST_LOBBY_HOSTS |
            NetRelayMessageType::RELAY_MSG_UNKNOWN => {
                return Err(NetRelayMessageError::UnknownType(head));
            }
        };
        
        Ok(result)
    }

    fn write_header(buf: &mut BytesMut, msg_type: NetRelayMessageType) {
        let mut header = (msg_type as u32) & 0xFF_FFFF;
        header |= ((NET_RELAY_PROTOCOL_VERSION & 0xFF) as u32) << 24;
        buf.put_u32_le(header);
    }
    
    fn into_bytes_mut(self) -> Result<BytesMut, io::Error> {
        let mut buf = BytesMut::with_capacity(128);
        
        match self {
            NetRelayMessage::Close(code) => {
                Self::write_header(&mut buf, NetRelayMessageType::RELAY_MSG_CLOSE);
                buf.put_u32_le(code);
            },
            NetRelayMessage::RelayPing(stamp) => {
                Self::write_header(&mut buf, NetRelayMessageType::RELAY_MSG_RELAY_PING);
                buf.put_u64_le(stamp.0);
                buf.put_u32_le(stamp.1);
            },
            NetRelayMessage::RelayListLobbyHosts { hosts, format } => {
                Self::write_header(&mut buf, NetRelayMessageType::RELAY_MSG_RELAY_LIST_LOBBY_HOSTS);
                let xprm = format == 1;
                let result = serialize_xprm_or_json(xprm, &hosts)
                    .map_err(|err| io::Error::other(err))?;
                buf.put_slice(result.as_bytes());
            },
            NetRelayMessage::RelayListLobbies { lobbies, format } => {
                Self::write_header(&mut buf, NetRelayMessageType::RELAY_MSG_RELAY_LIST_LOBBIES);
                let xprm = format == 1;
                let result = serialize_xprm_or_json(xprm, &lobbies)
                    .map_err(|err| io::Error::other(err))?;
                buf.put_slice(result.as_bytes());
            },
            NetRelayMessage::RelayListPeers(peers) => {
                Self::write_header(&mut buf, NetRelayMessageType::RELAY_MSG_RELAY_LIST_PEERS);
                buf.put_u32_le(peers.len() as u32);
                for peer in peers {
                    buf.put_u64_le(peer);
                }
            }
            NetRelayMessage::RelayAddPeer(netid) => {
                Self::write_header(&mut buf, NetRelayMessageType::RELAY_MSG_RELAY_ADD_PEER);
                buf.put_u64_le(netid);
            }
            NetRelayMessage::RelayRemovePeer(netid) => {
                Self::write_header(&mut buf, NetRelayMessageType::RELAY_MSG_RELAY_REMOVE_PEER);
                buf.put_u64_le(netid);
            }
            msg => {
                return Err(io::Error::other(format!("Unknown message type: {:?}", msg)));
            }
        }

        Ok(buf)
    }
}

impl TryFrom<NetRelayMessage> for BytesMut {
    type Error = io::Error;

    fn try_from(value: NetRelayMessage) -> Result<Self, Self::Error> {
        value.into_bytes_mut()
    }
}

impl TryFrom<NetRelayMessage> for Bytes {
    type Error = io::Error;

    fn try_from(value: NetRelayMessage) -> Result<Self, Self::Error> {
        BytesMut::try_from(value).map(|v| v.freeze())
    }
}

impl TryFrom<NetConnectionMessage> for NetRelayMessage {
    type Error = io::Error;

    fn try_from(value: NetConnectionMessage) -> Result<Self, Self::Error> {
        NetRelayMessage::from_message(value)
            .map_err(io::Error::other)
    }
}

impl Display for NetRelayMessageType {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl Display for NetRelayMessageError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl std::error::Error for NetRelayMessageError {}