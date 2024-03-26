use std::{error, io};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use actix_http::ws;
use actix_http::ws::{Frame, HandshakeError, Message, ProtocolError};
use actix_web::{
    web, HttpRequest, HttpResponse, HttpResponseBuilder
};
use actix_web::http::{header, Method, StatusCode};
use actix_web::http::header::HeaderValue;
use futures_util::{Sink, Stream, StreamExt, TryStreamExt};
use pin_project_lite::pin_project;
use tokio::sync::mpsc;
use tokio::time::interval;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::bytes::{Bytes, BytesMut};
use tokio_util::codec::{Decoder, Encoder, FramedRead};
use tokio_util::io::StreamReader;
use tokio_util::sync::{CancellationToken, PollSender};
use crate::AppData;
use crate::netconnection::codec::{NetConnectionCodecDecoder, NetConnectionCodecEncoder};
use crate::netconnection::{NetConnection, NetConnectionMessage, StreamMessage};
use crate::utils::config_from_str;

pub async fn handler(req: HttpRequest, payload: web::Payload, app_data: web::Data<AppData>) -> Result<HttpResponse, actix_web::Error> {
    let mut res = handshake_with_protocols(&req, &[])?;
    let info = if let Some(addr) = req.peer_addr() {
        log::info!("Incoming WebSocket connection: {:}", addr);
        format!("WebSocket({})", addr)
    } else {
        "WebSocket(?)".to_string()
    };

    //Codec for encoding and decoding WebSocket
    let codec = ws::Codec::new().max_size(NetConnectionMessage::MESSAGE_MAX_SIZE + 1024);
    
    //Shared cancellation token so the awaits don't get stuck
    let cancellation = CancellationToken::new();
    
    let buffer_size = config_from_str::<usize>("SERVER_CONNECTION_BUFFER_SIZE", 50);
    //To feed the NetConnectionStream
    let (msg_stream_tx, msg_stream_rx) = mpsc::channel(buffer_size);
    //To feed the HTTP response with WS messages to client
    let (ws_sink_tx, ws_sink_rx) = mpsc::channel(buffer_size);
    
    //Transform request payload into a stream which is fed into framed_read with WS codec
    let stream = StreamReader::new(
        payload.map_err(|err| io::Error::other(err))
    );
    let frame_stream = Box::pin(FramedRead::new(stream, codec.clone()));
    
    //Create task that will handle stream of incoming WS frames
    actix_web::rt::spawn(task_stream_handler(
        info.clone(),
        cancellation.clone(),
        frame_stream,
        ws_sink_tx.clone(),
        msg_stream_tx.clone(),
    ));

    //Wrap the websocket sink sender as a sink and add mapper to feed NetConnectionMessages
    let ws_sink = WebSocketSink::new(
        info.clone(),
        cancellation.clone(),
        PollSender::new(ws_sink_tx.clone())
    );

    //Plug the ws_sink_rx end into response's streaming body to feed WS messages to client as bytes
    let response = res.streaming(WebSocketStreamingBody::new(
        info.clone(), cancellation.clone(),
        ws_sink_rx, codec
    ));

    //Handle the rest at new task
    tokio::spawn(async move {
        //Spin websocket ping task
        let ping_interval = Duration::from_millis(config_from_str::<u64>("SERVER_WS_PING_INTERVAL", 5000));
        tokio::spawn(task_sink_ping(
            info.clone(), cancellation.clone(),
            ping_interval, ws_sink_tx
        ));
        
        //Create connection with both sink and stream
        let mut connection = NetConnection::from_streams(
            info,
            cancellation,
            Box::pin(ws_sink),
            Box::pin(ReceiverStream::new(msg_stream_rx))
        );
        let ws_timeout = config_from_str::<u64>("SERVER_WS_TIMEOUT", 10000);
        connection.stream_mut().set_timeout(Duration::from_millis(ws_timeout));
        app_data.session_manager.incoming_connection(connection).await;
    });

    Ok(response)
}

/// Prepare WebSocket handshake response.
///
/// This function returns handshake `HttpResponse`, ready to send to peer. It does not perform
/// any IO.
///
/// `protocols` is a sequence of known protocols. On successful handshake, the returned response
/// headers contain the first protocol in this list which the server also knows.
fn handshake_with_protocols(req: &HttpRequest, protocols: &[&str]) -> Result<HttpResponseBuilder, HandshakeError> {
    // WebSocket accepts only GET
    if *req.method() != Method::GET {
        return Err(HandshakeError::GetMethodRequired);
    }

    // check for "UPGRADE" to WebSocket header
    let has_hdr = if let Some(hdr) = req.headers().get(&header::UPGRADE) {
        if let Ok(s) = hdr.to_str() {
            s.to_ascii_lowercase().contains("websocket")
        } else {
            false
        }
    } else {
        false
    };
    if !has_hdr {
        return Err(HandshakeError::NoWebsocketUpgrade);
    }

    // Upgrade connection
    if !req.head().upgrade() {
        return Err(HandshakeError::NoConnectionUpgrade);
    }

    // check supported version
    if !req.headers().contains_key(&header::SEC_WEBSOCKET_VERSION) {
        return Err(HandshakeError::NoVersionHeader);
    }
    let supported_ver = {
        if let Some(hdr) = req.headers().get(&header::SEC_WEBSOCKET_VERSION) {
            hdr == "13" || hdr == "8" || hdr == "7"
        } else {
            false
        }
    };
    if !supported_ver {
        return Err(HandshakeError::UnsupportedVersion);
    }

    // check client handshake for validity
    if !req.headers().contains_key(&header::SEC_WEBSOCKET_KEY) {
        return Err(HandshakeError::BadWebsocketKey);
    }
    let key = {
        let key = req.headers().get(&header::SEC_WEBSOCKET_KEY).unwrap();
        ws::hash_key(key.as_ref())
    };

    // check requested protocols
    let protocol = req
        .headers()
        .get(&header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|req_protocols| {
            let req_protocols = req_protocols.to_str().ok()?;
            req_protocols
                .split(',')
                .map(|req_p| req_p.trim())
                .find(|req_p| protocols.iter().any(|p| p == req_p))
        });

    let mut response = HttpResponse::build(StatusCode::SWITCHING_PROTOCOLS)
        .upgrade("websocket")
        .insert_header((
            header::SEC_WEBSOCKET_ACCEPT,
            // key is known to be header value safe ascii
            HeaderValue::from_bytes(&key).unwrap(),
        ))
        .take();

    if let Some(protocol) = protocol {
        response.insert_header((header::SEC_WEBSOCKET_PROTOCOL, protocol));
    }

    Ok(response)
}

/// Task that periodically sends WS Ping messages to WS sink
async fn task_sink_ping(
    mut info: String,
    cancellation: CancellationToken,
    ping_interval: Duration,
    ws_sink_tx: mpsc::Sender<Message>,
) {
    info = format!("WebSocket::task_sink_ping {}", info);
    let mut interval = interval(ping_interval);
    let data = Bytes::new();
    while !cancellation.is_cancelled() {
        if let Err(err) = ws_sink_tx.send(Message::Ping(data.clone())).await {
            log::trace!("{:} send ping message error {:}", info, err);
            break;
        }
        
        //Eepy time, also important to let this loop don't block the async
        tokio::select! {
            _ = interval.tick() => {}
            _ = cancellation.cancelled() => {}
        }
    }
    log::trace!("{:} task finished", info);
    cancellation.cancel();
}

/// Task handling the incoming WS messages that are decoded into NetConnectionMessages if relevant
/// or sent to WS sink to answer the WS client (like Ping-Pong)
async fn task_stream_handler<S>(
    mut info: String,
    cancellation: CancellationToken,
    mut stream: S,
    ws_sink_tx: mpsc::Sender<Message>,
    stream_messages_tx: mpsc::Sender<io::Result<StreamMessage>>,
) where
    S: Stream<Item=Result<Frame, ProtocolError>> + Unpin
{
    let mut nc_decoder = NetConnectionCodecDecoder::new();
    info = format!("WebSocket::task_stream_start {}", info);
    loop {
        let frame = match tokio::select! {
            res = stream.next() => { res },
            _ = cancellation.cancelled() => { break; }
        } {
            Some(Ok(frame)) => frame,
            None => {
                log::trace!("{:} got closed", info);
                break;
            },
            Some(Err(err)) => {
                log::error!("{:} got error: {}", info, err);
                break;
            },
        };
        let stream_msg = match frame {
            Frame::Text(_) => {
                Err(io::Error::other(format!("{:} got unsupported frame Text", info)))
            },
            Frame::Binary(data) => {
                let mut data_mut = BytesMut::with_capacity(data.len());
                data_mut.copy_from_slice(data.as_ref());
                match nc_decoder.decode_eof(&mut data_mut) {
                    Err(err) => Err(err),
                    //We do not expect new data to arrive
                    //if we fail to create a NetConnectionMessage something is wrong
                    Ok(None) => Err(io::ErrorKind::InvalidData.into()),
                    Ok(Some(msg)) => Ok(StreamMessage::Message(msg)),
                }
            },
            Frame::Continuation(_) => {
                Err(io::Error::other(format!("{:} got unsupported frame Continuation", info)))
            },
            Frame::Ping(s) => {
                if let Err(err) = ws_sink_tx.send(Message::Pong(s)).await {
                    Err(io::Error::other(format!("{:} send stream message error {:}", info, err)))
                } else {
                    continue;
                }
            },
            Frame::Pong(_) => {
                Ok(StreamMessage::PingResponse)
            },
            Frame::Close(reason) => {
                log::debug!("{:} got Close frame reason: {:?}", info, reason);
                break;
            },
        };
        let error_occurred = stream_msg.is_err();
        if let Err(err) = stream_messages_tx.send(stream_msg).await {
            log::trace!("{:} send stream message error {:}", info, err);
        }
        //We sent error, nothing more to do here so break the loop
        if error_occurred {
            break;
        }
    }
    log::trace!("{:} task finished", info);
    cancellation.cancel();
}

pin_project! {
    /// Sink to transform NetConnectionMessage into websocket Message's that are sent to sender
    #[derive(Debug, Clone)]
    #[must_use = "sinks do nothing unless polled"]
    pub struct WebSocketSink<Si> {
        info: String,
        cancellation: CancellationToken,
        #[pin]
        sink: Si,
    }
}

impl<Si> WebSocketSink<Si> {
    pub fn new(info: String, cancellation: CancellationToken, sink: Si) -> Self {
        Self {
            info,
            cancellation,
            sink,
        }
    }
}

impl<Si, SiE> Sink<NetConnectionMessage> for WebSocketSink<Si>
    where
        SiE: Into<Box<dyn error::Error + Send + Sync>>,
        Si: Sink<Message, Error = SiE>
{
    type Error = io::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.as_mut().project().sink.poll_ready(cx)
            .map_err(|err| io::Error::other(err))
    }

    fn start_send(mut self: Pin<&mut Self>, item: NetConnectionMessage) -> Result<(), Self::Error> {
        let mut data = BytesMut::new();
        NetConnectionCodecEncoder::encode_message(item, &mut data)?;
        self.as_mut().project().sink.start_send(Message::Binary(data.freeze()))
            .map_err(|err| io::Error::other(err))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.as_mut().project().sink.poll_flush(cx)
            .map_err(|err| io::Error::other(err))
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let result = self.as_mut().project().sink.poll_close(cx)
            .map_err(|err| io::Error::other(err));
        log::trace!("WebSocketSink {:} closed", self.info);
        self.cancellation.cancel();
        result
    }
}

///Takes WS messages and encodes it into a Stream of Bytes's
struct WebSocketStreamingBody {
    info: String,
    cancellation: CancellationToken,
    receiver: mpsc::Receiver<Message>,
    codec: ws::Codec,
    buf: BytesMut,
}

impl WebSocketStreamingBody {
    fn new(info: String, cancellation: CancellationToken, receiver: mpsc::Receiver<Message>, codec: ws::Codec) -> Self {
        Self {
            info,
            cancellation,
            receiver,
            codec,
            buf: BytesMut::new(),
        }
    }
}

impl Stream for WebSocketStreamingBody {
    type Item = Result<Bytes, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        //Pull a msg if any and encode it
        if !this.cancellation.is_cancelled() {
            match this.receiver.poll_recv(cx) {
                Poll::Pending => {},
                Poll::Ready(None) => {
                    //Channel is closed and no message will come
                    log::trace!("WebSocketStreamingBody {:} closed", this.info);
                    this.cancellation.cancel();
                }
                Poll::Ready(Some(msg)) => {
                    this.codec.encode(msg, &mut this.buf)
                        .map_err(|err| io::Error::other(err))?;
                }
            }
        }
        
        //Send encoded bytes if any or send None/Pending
        if !this.buf.is_empty() {
            Poll::Ready(Some(Ok(this.buf.split().freeze())))
        } else if this.cancellation.is_cancelled() {
            Poll::Ready(None)
        } else {
            Poll::Pending
        }
    }
}
