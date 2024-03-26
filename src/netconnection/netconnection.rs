use std::{fmt, io};
use std::fmt::Debug;
use std::mem::size_of;
use std::pin::Pin;
use std::time::{Duration, Instant};
use futures_util::{FutureExt, Sink, SinkExt, Stream, StreamExt};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio_util::bytes::Bytes;
use tokio_util::sync::CancellationToken;
use crate::relay::messages::NetRelayMessageRelay;
use crate::utils::config_from_str;

pub type NETID = u64;
pub const NETID_NONE: NETID = 0;
pub const NETID_HOST: NETID = 1;
pub const NETID_RELAY: NETID = 2;
pub const NETID_ALL: NETID = 3;
/// The first NETID to start with for room NETIDs
/// Number is high to ensure games can allocate lower NETIDs for local use if required
pub const NETID_CLIENTS_RELAY_START: NETID = 0x10000;

pub enum StreamMessage {
    PingResponse,
    Message(NetConnectionMessage)
}

pub type NetConnectionMessageStream = Pin<Box<dyn Stream<Item = io::Result<StreamMessage>> + Send + Sync + 'static>>;
pub type NetConnectionMessageSink = Pin<Box<dyn Sink<NetConnectionMessage, Error = io::Error> + Send + Sync + 'static>>;

#[derive(Debug)]
pub struct NetConnectionMessage {
    ///The message's source NETID
    pub source_netid: NETID,
    ///The message's intended destination NETID
    pub destination_netid: NETID,
    ///Is the data compressed?
    pub compressed: bool,
    ///Actual data contained in the message
    pub data: Bytes,
}

impl NetConnectionMessage {
    pub const MESSAGE_MAX_SIZE: usize = 32 * 1024 * 1024;
    //const MESSAGE_COMPRESSION_SIZE: usize = 128 * 1024;
    ///Specifies this message contains compressed payload
    pub const FLAG_COMPRESSED: u16 = 1 << 0;
    pub const HEADER_SIZE: usize = 8;
    pub const HEADER_MAGIC: u64 = 0xDE000000000000CA;
    pub const HEADER_MASK: u64  = 0xFF000000000000FF;
    pub const SIZE_OF_NETID: usize = size_of::<NETID>();

    pub fn generate_header(&self) -> Result<(u64, usize), usize> {
        //Get data and total message length
        let mut data_len = self.data.len();
        data_len += Self::SIZE_OF_NETID * 2;
        let total_len = data_len + Self::HEADER_SIZE;

        //Check msg max size
        if 0 == total_len || total_len > Self::MESSAGE_MAX_SIZE {
            return Err(total_len);
        }

        //Set header
        let mut flags: u16 = 0;
        if self.compressed {
            flags |= Self::FLAG_COMPRESSED;
        }
        let mut header: u64 = Self::HEADER_MAGIC;
        header |= (flags as u64) << 8;
        header |= (data_len as u64) << 24;
        return Ok((header, total_len));
    }
}

pub enum NetConnectionRead<T> {
    Closed,
    Empty,
    Data(T)
}

pub struct NetConnectionStream {
    info: String,
    stream: NetConnectionMessageStream,
    last_contact: Instant,
    timeout: Duration,
    closed: CancellationToken,
}

impl Debug for NetConnectionStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("NetConnectionStream")
            .field(&self.info)
            .field(&self.last_contact)
            .field(&self.timeout)
            .field(&self.closed)
            .finish()
    }
}

impl NetConnectionStream {
    pub fn new(closed: CancellationToken, info: String, stream: NetConnectionMessageStream) -> Self {
        Self {
            stream,
            last_contact: Instant::now(),
            timeout: Duration::ZERO,
            closed,
            info,
        }
    }
    
    pub fn close(&mut self) {
        if !self.is_closed() {
            log::debug!("{:} closing", self);
        }
        self.closed.cancel();
    }
    
    pub fn is_closed(&self) -> bool {
        self.closed.is_cancelled()
    }
    
    pub async fn read_message(&mut self) -> NetConnectionRead<NetConnectionMessage> {
        if self.is_closed() {
            return NetConnectionRead::Closed;
        }
        match tokio::select! {
            res = self.stream.next() => { res },
            _ = self.closed.cancelled() => { None }
        } {
            None => {
                log::trace!("NetConnectionStream {:} got closed", self.info);
                self.close();
                NetConnectionRead::Closed
            },
            Some(Err(err)) => {
                log::error!("NetConnectionStream {:} got error: {}", self.info, err);
                self.close();
                NetConnectionRead::Closed
            },
            Some(Ok(StreamMessage::PingResponse)) => {
                self.update_last_contact();
                NetConnectionRead::Empty
            }
            Some(Ok(StreamMessage::Message(msg))) => {
                NetConnectionRead::Data(msg)
            },
        }
    }
    
    pub async fn read_message_or_timeout(&mut self, timeout: Duration) -> NetConnectionRead<NetConnectionMessage> {
        let sleep = tokio::time::sleep(timeout);
        tokio::pin!(sleep);
        tokio::select! {
            msg = self.read_message() => {
                msg
            }
            _ = &mut sleep => {
                NetConnectionRead::Empty
            }
        }
    }

    pub fn try_read_message(&mut self) -> NetConnectionRead<NetConnectionMessage> {
        if self.is_closed() {
            return NetConnectionRead::Closed;
        }
        self.read_message().now_or_never()
            .unwrap_or(NetConnectionRead::Empty)
    }

    pub fn try_read_messages(&mut self, max_msgs: usize) -> NetConnectionRead<Vec<NetConnectionMessage>> {
        let mut msgs = Vec::new();
        for i in 0..max_msgs {
            match self.try_read_message() {
                NetConnectionRead::Empty => {
                    if i == 0 { return NetConnectionRead::Empty; } else { break; }
                },
                NetConnectionRead::Closed => {
                    if i == 0 { return NetConnectionRead::Closed; } else { break; }
                }
                NetConnectionRead::Data(msg) => {
                    msgs.push(msg);
                }
            }
        }
        NetConnectionRead::Data(msgs)
    }

    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    pub fn update_last_contact(&mut self) {
        self.last_contact = Instant::now();
    }

    pub fn is_last_contact_more_than(&self, duration: Duration) -> bool {
        self.last_contact.elapsed() > duration
    }

    pub fn has_contact_timeout(&self) -> bool {
        !self.timeout.is_zero() && self.is_last_contact_more_than(self.timeout)
    }

}

#[derive(Debug)]
enum NetConnectionSinkMessage {
    CloseCode(u32),
    Flush,
    Send(NetConnectionMessage, bool),
}

#[derive(Debug)]
pub enum NetConnectionWrite {
    Closed,
    Full,
    IO(io::Error),
}

#[derive(Debug, Clone)]
pub struct NetConnectionSink {
    ///Flag to know connection stream is closed, we do not set it from sink as we may have several copies
    closed: CancellationToken,
    info: String,
    netid: NETID,
    queue_tx: mpsc::Sender<NetConnectionSinkMessage>,
}

impl NetConnectionSink {
    pub fn new(closed: CancellationToken, buffer_size: usize, info: String, sink: NetConnectionMessageSink) -> Self {
        let (queue_tx, queue_rx) = mpsc::channel(buffer_size);
        //Task that will poll the channel and send over sink, will finish once sink or channel is closed
        tokio::spawn(Self::task_entry(info.clone(), sink, queue_rx, closed.clone()));
        Self {
            closed,
            info,
            netid: NETID_NONE,
            queue_tx,
        }
    }

    async fn task_entry(
        info: String,
        mut sink: NetConnectionMessageSink,
        mut queue_rx: mpsc::Receiver<NetConnectionSinkMessage>,
        closed: CancellationToken
    ) {
        let mut close_code = 0;
        //Keep going until None is returned which means is closed + no more msgs in queue
        //We don't want to check closed state yet as we may have some pending messages left
        while let Some(action) = queue_rx.recv().await {
            if let Err(err) = match action {
                NetConnectionSinkMessage::CloseCode(code) => {
                    close_code = code;
                    Ok(())
                },
                NetConnectionSinkMessage::Flush => sink.flush().await,
                NetConnectionSinkMessage::Send(msg, flush) => {
                    let res = sink.feed(msg).await;
                    if res.is_err() {
                        res
                    } else if flush {
                        sink.flush().await
                    } else {
                        Ok(())
                    }
                },
            } {
                log::error!("NetConnectionSink::task action error {:?}", err);
                break;
            }
        }
        //Send close message, close sink and set flag
        match Bytes::try_from(NetRelayMessageRelay::Close(close_code)) {
            Err(err) => log::error!("NetConnectionSink::task send close error {:?}", err),
            Ok(data) => {
                let _ = sink.feed(NetConnectionMessage {
                    source_netid: NETID_RELAY,
                    destination_netid: NETID_NONE,
                    compressed: false,
                    data,
                }).await;
            }
        }
        let _ = sink.flush().await;
        let _ = sink.close().await;
        log::trace!("NetConnectionSink::task {:} finished", info);
        closed.cancel();
    }

    pub fn is_closed(&self) -> bool {
        self.closed.is_cancelled()
    }

    pub fn get_netid(&self) -> NETID {
        self.netid
    }

    pub fn set_netid(&mut self, netid: NETID) {
        log::info!("{:} set NETID {:}", self, netid);
        self.netid = netid;
    }

    fn try_send(&self, msg: NetConnectionSinkMessage) -> Result<(), NetConnectionWrite> {
        if self.is_closed() {
            return Err(NetConnectionWrite::Closed);
        }
        self.queue_tx.try_send(msg).map_err(|err| {
            match err {
                TrySendError::Full(_) => NetConnectionWrite::Full,
                TrySendError::Closed(_) => NetConnectionWrite::Closed,
            }
        })
    }

    pub fn set_close_code(&self, code: u32) -> Result<(), NetConnectionWrite> {
        self.try_send(NetConnectionSinkMessage::CloseCode(code))
    }

    #[allow(dead_code)]
    pub async fn try_send_message(&self, msg: NetConnectionMessage, flush: bool) -> Result<(), NetConnectionWrite> {
        self.try_send(NetConnectionSinkMessage::Send(msg, flush))
    }

    async fn send(&self, msg: NetConnectionSinkMessage) -> Result<(), NetConnectionWrite> {
        if self.is_closed() {
            return Err(NetConnectionWrite::Closed);
        }
        if let Err(_) = self.queue_tx.send(msg).await {
            Err(NetConnectionWrite::Closed)
        } else {
            Ok(())
        }
    }

    pub async fn flush(&self) -> Result<(), NetConnectionWrite> {
        self.send(NetConnectionSinkMessage::Flush).await
    }
    
    pub async fn send_message(&self, msg: NetConnectionMessage, flush: bool) -> Result<(), NetConnectionWrite> {
        self.send(NetConnectionSinkMessage::Send(msg, flush)).await
    }

    pub async fn send_relay_message(&self, msg: NetRelayMessageRelay, flush: bool) -> Result<(), NetConnectionWrite> {
        let data = Bytes::try_from(msg).map_err(|err| NetConnectionWrite::IO(err))?;
        self.send_message(
            NetConnectionMessage {
                source_netid: NETID_RELAY,
                destination_netid: self.netid,
                compressed: false,
                data,
            },
            flush
        ).await        
    }
}

#[derive(Debug)]
pub struct NetConnection {
    sink: NetConnectionSink,
    stream: NetConnectionStream,
}

impl NetConnection {
    pub fn from_parts(sink: NetConnectionSink, stream: NetConnectionStream) -> Self {
        Self {
            sink,
            stream,
        }
    }

    pub fn from_streams(info: String, closed: CancellationToken, sink: NetConnectionMessageSink, stream: NetConnectionMessageStream) -> Self {
        let buffer_size = config_from_str::<usize>("SERVER_CONNECTION_BUFFER_SIZE", 50);
        Self::from_parts(
            NetConnectionSink::new(closed.clone(), buffer_size, info.clone(), sink),
            NetConnectionStream::new(closed, info, stream)
        )
    }

    #[allow(dead_code)]
    pub fn into_streams(self) -> (NetConnectionSink, NetConnectionStream) {
        (self.sink, self.stream)
    }

    pub fn get_netid(&self) -> NETID {
        self.sink.get_netid()
    }

    pub fn set_netid(&mut self, netid: NETID) {
        log::info!("{:} set NETID {:}", self, netid);
        self.sink.set_netid(netid);
    }

    pub fn sink(&self) -> &NetConnectionSink {
        &self.sink
    }

    pub fn stream(&self) -> &NetConnectionStream {
        &self.stream
    }

    pub fn stream_mut(&mut self) -> &mut NetConnectionStream {
        &mut self.stream
    }

    pub fn clone_sink(&self) -> NetConnectionSink {
        self.sink.clone()
    }

    pub fn is_closed(&self) -> bool {
        self.stream.is_closed() || self.sink.is_closed()
    }
    
    pub fn close(&mut self, code: u32) {
        if !self.is_closed() {
            log::debug!("{:} closing", self);
        }
        let _ = self.sink.set_close_code(code);
        self.stream.close();
    }
}

impl fmt::Display for NetConnectionMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "NetConnectionMessage {{ Src/Dst: {} -> {}, Compressed: {} DataLen: {} }}",
            self.source_netid,
            self.destination_netid,
            self.compressed,
            self.data.len()
        )
    }
}

impl fmt::Display for NetConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.sink.netid == 0 {
            write!(f, "NetConnection {{ NETID: {}, Sink: {}, Seen: {}ms }}",
                   self.sink.netid,
                   self.sink.info,
                   self.stream.last_contact.elapsed().as_millis(),
            )            
        } else {
            write!(f, "NetConnection {{ NETID: {}, Seen: {}ms }}",
                   self.sink.netid,
                   self.stream.last_contact.elapsed().as_millis(),
            )
        }
    }
}

impl fmt::Display for NetConnectionWrite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl fmt::Display for NetConnectionStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NCStream {{ Info: {} }}",
               self.info,
        )
    }
}

impl fmt::Display for NetConnectionSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NCSink {{ NETID: {}, Info: {} }}",
               self.netid,
               self.info,
        )
    }
}

impl Drop for NetConnectionStream {
    fn drop(&mut self) {
        self.close();
    }
}
