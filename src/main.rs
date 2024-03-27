extern crate core;

mod utils;
mod http;
mod lobby;
mod relay;
mod netconnection;
#[macro_use]
mod macros;

use std::io;
use std::net::SocketAddr;
use std::str::FromStr;
use futures_util::TryStreamExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;
use actix_web::{App, HttpServer};
use actix_web::http::KeepAlive;
use actix_web::middleware::Logger;
use actix_web::web::Data;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use crate::lobby::room_info::LobbyHost;
use crate::netconnection::{codec, NetConnection, StreamMessage};
use crate::relay::session_manager::{SessionManager, SessionManagerSender};
use crate::utils::{config_from_str, config_string};

pub struct AppData {
    lobby_host: LobbyHost,
    allowed_games_types: Vec<String>,
    room_infos: lobby::lobby_manager::CurrentRoomInfos,
    session_manager: SessionManagerSender,
}

fn run_http_server(http_bind_address: String, appdata: Arc<AppData>) -> std::io::Result<JoinHandle<std::io::Result<()>>> {
    let logger_format = config_string("SERVER_LOGGER_FORMAT", "[%a][%{r}a] \"%r\" %s %b %D \"%{User-Agent}i\"");
    let workers = config_from_str::<usize>("SERVER_WORKERS", 0);
    let workers_blocking = config_from_str::<usize>("SERVER_WORKERS_BLOCKING", 0);
    let keep_alive = config_from_str::<u64>("SERVER_KEEP_ALIVE", 0);

    //We create a separate thread to run the actix system since we can't run inside another runtime
    match std::thread::Builder::new()
        .name("actix_system".to_string())
        .spawn(move || {
            //Launch the actix system and web server
            log::info!("Binding HTTP listener at: {}", http_bind_address);
            let system = actix_web::rt::System::new();
            system.block_on(async move {
                let mut server = HttpServer::new(move || {
                    App::new()
                        .app_data(Data::from(appdata.clone()))
                        .configure(http::configure)
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

pub async fn tcp_listener(session_manager: SessionManagerSender) -> Result<(), io::Error> {
    let tcp_address = config_string("SERVER_TCP_BIND_ADDRESS", "0.0.0.0:11654");
    let tcp_linger = config_from_str::<u64>("SERVER_TCP_LINGER", 1);
    let tcp_timeout = config_from_str::<u64>("SERVER_TCP_TIMEOUT", 10000);
    let socketaddr = match SocketAddr::from_str(tcp_address.as_str()) {
        Ok(a) => a,
        Err(err) => {
            log::error!("TCP listener unknown address format: {:} error: {:}", tcp_address, err);
            return Err(io::Error::other(err));
        }
    };
    log::info!("Binding TCP listener at: {:}", socketaddr);
    let listener = match TcpListener::bind(socketaddr).await {
        Ok(l) => {
            l
        }
        Err(err) => {
            log::error!("Couldn't bind relay to address! {:} {:?}", socketaddr, err);
            return Err(err);
        }
    };
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, addr)) =>  {
                    //Handle connection on a new task to avoid blocking the socket acceptor task
                    let session_manager_task = session_manager.clone();
                    tokio::spawn(async move {
                        log::info!("Incoming TCP connection: {:}", addr);
                        if let Err(err) = stream.set_nodelay(true) {
                            log::error!("TCP {:} error setting nodelay: {:}", addr, err);
                        }
                        if let Err(err) = stream.set_linger(Some(Duration::from_secs(tcp_linger))) {
                            log::error!("TCP {:} error setting linger: {:}", addr, err);
                        }
                        let (read, write) = stream.into_split();
                        let stream = codec::NetConnectionCodecDecoder::new_framed(read)
                            .map_ok(|msg| StreamMessage::Message(msg));
                        let mut connection = NetConnection::from_streams(
                            format!("TCP({})", addr),
                            CancellationToken::new(),
                            Box::pin(codec::NetConnectionCodecEncoder::new_framed(write)),
                            Box::pin(stream)
                        );
                        connection.stream_mut().set_timeout(Duration::from_millis(tcp_timeout));
                        session_manager_task.incoming_connection(connection).await;
                    });
                }
                Err(err) => {
                    log::info!("TCP listener accept error: {:}", err);
                    return Result::<(), io::Error>::Err(err)
                }
            }
        }
    });

    Ok(())
}

async fn setup_app() -> std::io::Result<()> {
    let allowed_games_types: Vec<String> = dotenvy::var("SERVER_ALLOWED_GAME_TYPES")
        .unwrap_or_default()
        .split(",")
        .filter_map(|v| if v.is_empty() { None } else { Some(String::from(v)) })
        .collect();
    let tcp_public_address = config_string("SERVER_TCP_PUBLIC_ADDRESS", "127.0.0.1:11654");
    let http_bind_address = config_string("SERVER_HTTP_BIND_ADDRESS", "0.0.0.0:8888");
    let mut http_public_address = config_string("SERVER_HTTP_PUBLIC_ADDRESS", http_bind_address.as_str());
    if !http_public_address.starts_with("http") && !http_public_address.contains("://") {
        http_public_address = "http://".to_string() + http_public_address.as_str();
    }
    let ws_public_address = config_string("SERVER_WS_PUBLIC_ADDRESS", (http_public_address.replace("http", "ws")).as_str());
    log::info!(
        "Public relay address:\n\tHTTP = {}\n\tWS = {}\n\tTCP = {}",
        http_public_address,
        ws_public_address,
        tcp_public_address
    );
        
    //Create shared app state for request handlers
    let (session_manager_queue_tx, session_manager_queue_rx) = SessionManagerSender::new();
    let app_data = Arc::new(AppData {
        lobby_host: LobbyHost {
            host_tcp: tcp_public_address,
            host_ws: ws_public_address,
        },
        allowed_games_types,
        room_infos: Default::default(),
        session_manager: session_manager_queue_tx,
    });

    //Setup relay
    SessionManager::init(session_manager_queue_rx, app_data.clone());
    app_data.session_manager.ping().await;

    //Start relay listener
    if let Err(err) = tcp_listener(app_data.session_manager.clone()).await {
        return Err(err);
    }

    //Spin main HTTP server and wait, will finish if SIGINT is received;
    let actix_thread = run_http_server(http_bind_address, app_data)?;
    actix_thread.join().unwrap_or_else(|err| {
        log::error!("Error occurred joining actix system thread: {:?}", err);
        Err(io::Error::other("Thread JoinHandle error"))
    })
}

fn main() -> io::Result<()> {
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
