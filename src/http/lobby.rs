use actix_web::{get, HttpRequest, Responder};
use actix_web::web::{Data, Query};
use serde::{Deserialize, Serialize};
use crate::AppData;
use crate::http::utils::http_ok_serialize;
use crate::lobby::lobby_manager::LobbyManager;
use crate::lobby::room_info::LobbyWithRooms;

#[derive(Serialize)]
struct LobbyListResponse<'a> {
    lobbies: &'a [LobbyWithRooms],
}

#[derive(Deserialize)]
struct LobbyListQuery {
    game_type: Option<String>
}

#[get("/lobby/list")]
pub async fn list(app_data: Data<AppData>, query: Query<LobbyListQuery>, req: HttpRequest) -> impl Responder {
    let game_type = query.game_type.to_owned().unwrap_or_default();
    let room_infos_guard = &app_data.room_infos.load();
    let rooms = LobbyManager::filter_room_by_game_type(room_infos_guard, &game_type);
    let response = LobbyListResponse {
        lobbies: &[
            LobbyWithRooms {
                host: app_data.lobby_host.clone(),
                rooms,
            }
        ]
    };
    http_ok_serialize(&req, response)
}

#[get("/lobby/game_types")]
pub async fn game_types(data: Data<AppData>, req: HttpRequest) -> impl Responder {
    http_ok_serialize(&req, &data.allowed_games_types)
}
