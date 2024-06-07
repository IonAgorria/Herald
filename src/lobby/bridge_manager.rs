use std::collections::HashMap;
use crate::lobby::room_info::RoomID;

struct BridgedRoom {
    id: RoomID,
    
}

pub struct BridgeManager {
    rooms: HashMap<RoomID, BridgedRoom>,
}