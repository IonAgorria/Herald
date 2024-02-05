use std::io::Error;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use crate::AppData;
use crate::relay::session_manager::{SessionManager, SessionManagerSender};
use crate::relay::netconnection::NetConnection;
use crate::utils::config_string;

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
    let socketaddr = match SocketAddr::from_str(tcp_address.as_str()) {
        Ok(a) => a,
        Err(err) => {
            log::error!("TCP listener unknown address format: {:} error: {:}", tcp_address, err);
            return Err(Error::other(err));
        }
    };
    let listener = match TcpListener::bind(socketaddr).await {
        Ok(l) => {
            log::info!("Listening relay at: {:}", socketaddr);
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
                    let connection = NetConnection::from_transport_tcp(addr, stream);
                    log::info!("TCP Incoming connection {:}", connection);
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
