use std::io::Error;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use crate::AppData;
use crate::netconnection::{NetConnection, codec};
use crate::relay::session_manager::{SessionManager, SessionManagerSender};
use crate::utils::{config_from_str, config_string};

pub async fn init(
    app_data: Arc<AppData>,
) -> Result<(), Error> {
    let session_manager = SessionManager::init(app_data);
    session_manager.queue_tx.ping().await;

    if let Err(err) = tcp_listener(session_manager.queue_tx).await {
        return Err(err);
    }
    Ok(())
}

async fn tcp_listener(session_manager: SessionManagerSender) -> Result<JoinHandle<Result<(), Error>>, Error> {
    let tcp_address = config_string("SERVER_TCP_BIND_ADDRESS", "0.0.0.0:11654");
    let tcp_linger = config_from_str::<u64>("SERVER_TCP_LINGER", 1);
    let tcp_timeout = config_from_str::<u64>("SERVER_TCP_TIMEOUT", 10000);
    let socketaddr = match SocketAddr::from_str(tcp_address.as_str()) {
        Ok(a) => a,
        Err(err) => {
            log::error!("TCP listener unknown address format: {:} error: {:}", tcp_address, err);
            return Err(Error::other(err));
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
    let handle = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, addr)) =>  {
                    log::info!("TCP Incoming connection {:}", addr);
                    if let Err(err) = stream.set_nodelay(true) {
                        log::error!("TCP {:} error setting nodelay: {:}", addr, err);
                    }
                    if let Err(err) = stream.set_linger(Some(Duration::from_secs(tcp_linger))) {
                        log::error!("TCP {:} error setting linger: {:}", addr, err);
                    }
                    let (read, write) = stream.into_split();
                    let mut connection = NetConnection::from_streams(
                        format!("TCP({})", addr),
                        Box::pin(codec::NetConnectionCodecEncoder::new_framed(write)),
                        Box::pin(codec::NetConnectionCodecDecoder::new_framed(read))
                    );
                    connection.set_timeout(Duration::from_millis(tcp_timeout));
                    session_manager.incoming_connection(connection).await;
                }
                Err(err) => {
                    log::info!("TCP listener accept error: {:}", err);
                    return Err(err);
                }
            }
        }
    });
    
    Ok(handle)
}
