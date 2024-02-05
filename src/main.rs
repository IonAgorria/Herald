extern crate core;

mod utils;
mod http;
mod lobby;
mod relay;
#[macro_use]
mod macros;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;
use actix_web::{App, HttpServer};
use actix_web::http::KeepAlive;
use actix_web::middleware::Logger;
use actix_web::web::Data;
use crate::utils::{config_from_str, config_string};

#[actix_web::get("/")]
async fn root() -> impl actix_web::Responder {
    http::utils::http_ok_utf8("🦀")
}

pub struct AppData {
    tcp_public_address: String,
    allowed_games_types: Vec<String>,
    room_infos: lobby::lobby_manager::CurrentRoomInfos,
}

fn run_http_server(appdata: Arc<AppData>) -> std::io::Result<JoinHandle<std::io::Result<()>>> {
    let logger_format = config_string("SERVER_LOGGER_FORMAT", "[%a][%{r}a] \"%r\" %s %b %D \"%{User-Agent}i\"");
    let http_bind_address = config_string("SERVER_HTTP_BIND_ADDRESS", "0.0.0.0:8888");
    let workers = config_from_str::<usize>("SERVER_WORKERS", 0);
    let workers_blocking = config_from_str::<usize>("SERVER_WORKERS_BLOCKING", 0);
    let keep_alive = config_from_str::<u64>("SERVER_KEEP_ALIVE", 0);

    //We create a separate thread to run the actix system since we can't run inside another runtime
    match std::thread::Builder::new()
        .name("actix_system".to_string())
        .spawn(move || {
            //Launch the actix system and web server
            log::info!("Listening HTTP at: {}", http_bind_address);
            let system = actix_web::rt::System::new();
            system.block_on(async move {
                let mut server = HttpServer::new(move || {
                    App::new()
                        .app_data(Data::from(appdata.clone()))
                        .configure(http::configure)
                        .service(root)
                        .wrap(Logger::new(logger_format.as_str()))
                        .wrap(actix_cors::Cors::permissive())
                });
                if 0 < workers {
                    server = server.workers(workers);
                }
                if 0 < workers_blocking {
                    server = server.worker_max_blocking_threads(workers_blocking);
                }
                if 0 < keep_alive {
                    server = server.keep_alive(KeepAlive::Timeout(Duration::from_millis(keep_alive)));
                }
                server.bind(http_bind_address)?.run().await
            })
        }) {
        Err(err) => {
            log::error!("Error occurred creating actix system thread: {:?}", err);
            Err(err)
        }
        Ok(actix_thread) => Ok(actix_thread)
    }
}

async fn setup_app() -> std::io::Result<()> {
    let allowed_games_types: Vec<String> = dotenvy::var("SERVER_ALLOWED_GAME_TYPES")
        .unwrap_or_default()
        .split(",")
        .filter_map(|v| if v.is_empty() { None } else { Some(String::from(v)) })
        .collect();
    let tcp_public_address = config_string("SERVER_TCP_PUBLIC_ADDRESS", "");
    if tcp_public_address.is_empty() {
        return Err(std::io::Error::other("SERVER_TCP_PUBLIC_ADDRESS not configured!"));
    }
    log::info!("Public relay address: {}", tcp_public_address);
    
    //Create shared app state for request handlers
    let appdata = Arc::new(AppData {
        tcp_public_address,
        allowed_games_types,
        room_infos: Default::default(),
    });
    
    //Setup relay
    relay::utils::init(appdata.clone()).await?;

    //Spin main HTTP server and wait, will finish if SIGINT is received;
    let actix_thread = run_http_server(appdata)?;
    actix_thread.join().unwrap_or_else(|err| {
        log::error!("Error occurred joining actix system thread: {:?}", err);
        Err(std::io::Error::other("Thread JoinHandle error"))
    })
}

fn main() -> std::io::Result<()> {
    //Setup env logger
    let rust_log = config_string("RUST_LOG", "INFO");
    env_logger::init_from_env(env_logger::Env::default().default_filter_or(rust_log.to_uppercase()));
    
    //Setup tokio runtime
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .thread_name_fn(|| {
            static ATOMIC_ID: AtomicUsize = AtomicUsize::new(0);
            let id = ATOMIC_ID.fetch_add(1, Ordering::SeqCst);
            format!("tokio-worker-{}", id)
        })
        .enable_all()
        .build() {
        Ok(runtime) => runtime,
        Err(err) => {
            log::error!("Error occurred creating runtime: {:?}", err);
            panic!();
        }
    };

    //Setup the app
    let result = runtime.block_on(setup_app());
    runtime.shutdown_timeout(Duration::from_secs(60));

    log::info!("Shutting down: {:?}", result);
    result
}
