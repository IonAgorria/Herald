use std::{fmt, io};
use std::mem::size_of;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};
use futures_util::{Sink, SinkExt, StreamExt};
use futures_util::stream::BoxStream;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::task::JoinHandle;
use tokio_util::bytes::Bytes;
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

type NetConnectionClosed = Arc<AtomicBool>;
pub type NetConnectionMessageStream = BoxStream<'static, io::Result<NetConnectionMessage>>;
pub type NetConnectionMessageSink = Pin<Box<dyn Sink<NetConnectionMessage, Error = io::Error> + Send + 'static>>;

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

#[derive(Debug)]
pub struct NetConnectionStream {
    closed: NetConnectionClosed,
    info: String,
    queue_rx: mpsc::Receiver<NetConnectionRead<NetConnectionMessage>>,
    ///Task that will poll the stream and send over channel, will finish once stream or channel is closed
    task: JoinHandle<()>,
}

impl NetConnectionStream {
    pub fn new(closed: NetConnectionClosed, buffer_size: usize, info: String, stream: NetConnectionMessageStream) -> Self {
        let (queue_tx, queue_rx) = mpsc::channel(buffer_size);
        let task = tokio::spawn(Self::task_entry(info.clone(), stream, queue_tx));
        Self {
            closed,
            info,
            queue_rx,
            task
        }
    }
    
    pub fn close(&mut self) {
        if !self.is_closed() {
            log::debug!("{:} closing", self);
        }
        self.closed.store(true, Relaxed);
        self.queue_rx.close();
        self.task.abort();
    }
    
    pub fn is_closed(&self) -> bool {
        self.closed.load(Relaxed)
    }
    
    async fn task_entry(
        info: String,
        mut stream: NetConnectionMessageStream,
        queue_tx: mpsc::Sender<NetConnectionRead<NetConnectionMessage>>
    ) {
        loop {
            let res = match stream.next().await {
                Some(Ok(msg)) => NetConnectionRead::Data(msg),
                None => {
                    log::trace!("NetConnectionStream::task {:} got closed", info);
                    break;
                },
                Some(Err(err)) => {
                    log::error!("NetConnectionStream::task {:} got error: {}", info, err);
                    break;
                },
            };
            if let Err(err) = queue_tx.send(res).await {
                log::trace!("NetConnectionStream::task {:} send error {:}", info, err);
                break;
            }
        }
        log::trace!("NetConnectionStream::task {:} task finished", info);
    }

    pub async fn read_message(&mut self) -> NetConnectionRead<NetConnectionMessage> {
        if self.is_closed() {
            return NetConnectionRead::Closed;
        }
        //Returns None if closed, so just do this
        let res = self.queue_rx.recv().await
            .unwrap_or(NetConnectionRead::Closed);
        if let NetConnectionRead::Closed = res {
            self.close();
        }
        res
    }

    pub fn try_read_message(&mut self) -> NetConnectionRead<NetConnectionMessage> {
        if self.is_closed() {
            return NetConnectionRead::Closed;
        }
        let res = match self.queue_rx.try_recv() {
            Err(mpsc::error::TryRecvError::Empty) => NetConnectionRead::Empty,
            Err(mpsc::error::TryRecvError::Disconnected) => NetConnectionRead::Closed,
            Ok(msg) => msg,
        };
        if let NetConnectionRead::Closed = res {
            self.close();
        }
        res
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
    closed: NetConnectionClosed,
    info: String,
    netid: NETID,
    queue_tx: mpsc::Sender<NetConnectionSinkMessage>,
}

impl NetConnectionSink {
    pub fn new(closed: NetConnectionClosed, buffer_size: usize, info: String, sink: NetConnectionMessageSink) -> Self {
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
        closed: NetConnectionClosed
    ) {
        let mut close_code = 0;
        //Keep going until None is returned which means is closed + no more msgs in queue
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
        closed.store(true, Relaxed);
        log::trace!("NetConnectionSink::task {:} finished", info);
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Relaxed)
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
    last_contact: Instant,
    timeout: Duration,
    sink: NetConnectionSink,
    stream: NetConnectionStream,
}

impl NetConnection {
    pub fn from_parts(sink: NetConnectionSink, stream: NetConnectionStream) -> Self {
        Self {
            last_contact: Instant::now(),
            timeout: Duration::ZERO,
            sink,
            stream,
        }
    }

    pub fn from_streams(info: String, sink: NetConnectionMessageSink, stream: NetConnectionMessageStream) -> Self {
        let buffer_size = config_from_str::<usize>("SERVER_CONNECTION_BUFFER_SIZE", 50);
        let closed = NetConnectionClosed::default();
        Self::from_parts(
            NetConnectionSink::new(closed.clone(), buffer_size, info.clone(), sink),
            NetConnectionStream::new(closed, buffer_size, info, stream)
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

    pub fn stream_mut(&mut self) -> &mut NetConnectionStream {
        &mut self.stream
    }

    pub fn clone_sink(&self) -> NetConnectionSink {
        self.sink.clone()
    }

    pub fn is_closed(&self) -> bool {
        self.stream.is_closed() || self.sink.is_closed()
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
                   self.last_contact.elapsed().as_millis(),
            )            
        } else {
            write!(f, "NetConnection {{ NETID: {}, Seen: {}ms }}",
                   self.sink.netid,
                   self.last_contact.elapsed().as_millis(),
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
