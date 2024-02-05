// A codec for NetConnection messages/frames delimited by a frame head specifying their lengths.
//
// Heavily based on tokio_util::codec::LengthDelimitedCodec

use tokio_util::codec;
use tokio_util::bytes::{Buf, BufMut, BytesMut};
use tokio::io::{AsyncRead, AsyncWrite};
use std::io;
use std::fmt;
use std::mem::{size_of, size_of_val};
use codec::{Decoder, Encoder};
use crate::relay::netconnection::{NetConnectionMessage, NETID};
 
const MESSAGE_HEADER_SIZE: usize = 8;
const MESSAGE_MAX_SIZE: usize = 32 * 1024 * 1024;
//const MESSAGE_COMPRESSION_SIZE: usize = 128 * 1024;
///Specifies this message contains compressed payload
pub const NETCONNECTION_MESSAGE_FLAG_COMPRESSED: u16 = 1 << 0;
const MESSAGE_HEADER_MAGIC: u64 = 0xDE000000000000CA;
const MESSAGE_HEADER_MASK: u64  = 0xFF000000000000FF;

const SIZE_OF_NETID: usize = size_of::<NETID>();

#[derive(Debug)]
struct HeaderParts {
    data_length: usize,
    flag_compressed: bool,
}

/// Contains errors produced by codec.
#[derive(Debug)]
pub enum NetConnectionCodecError {
    HeaderMagic(u64),
    WrongLength(usize),
    BytesRemaining(usize),
    UnknownState,
}

#[derive(Debug)]
pub struct NetConnectionCodecDecoder {
    // Read state, if None means header needs to be read, otherwise data
    header: Option<HeaderParts>,
}

#[derive(Debug)]
pub struct NetConnectionCodecEncoder;

#[derive(Debug)]
pub struct NetConnectionCodec {
    decoder: NetConnectionCodecDecoder,
    encoder: NetConnectionCodecEncoder,
}

impl NetConnectionCodec {
    pub fn new() -> Self {
        Self {
            decoder: NetConnectionCodecDecoder::new(),
            encoder: NetConnectionCodecEncoder::new(),
        }
    }
    
    #[allow(dead_code)]
    pub fn new_framed<T: AsyncRead + AsyncWrite>(inner: T) -> codec::Framed<T, Self> {
        codec::Framed::new(inner, Self::new())
    }
}

impl NetConnectionCodecDecoder {
    pub fn new() -> Self {
        Self {
            header: None,
        }
    }

    #[allow(dead_code)]
    pub fn new_framed<T: AsyncRead>(inner: T) -> codec::FramedRead<T, Self> {
        codec::FramedRead::new(inner, Self::new())
    }
    
    fn decode_head(&mut self, src: &mut BytesMut) -> io::Result<Option<HeaderParts>> {
        if src.len() < MESSAGE_HEADER_SIZE {
            // Not enough data
            return Ok(None);
        }
        
        //Get the header
        let header: u64 = src.get_u64();
        assert_eq!(size_of_val(&header), MESSAGE_HEADER_SIZE);
        if (header & MESSAGE_HEADER_MASK) != MESSAGE_HEADER_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                NetConnectionCodecError::HeaderMagic(header),
            ));
        }
        
        //Parse header
        let data_length: usize = ((header >> 24) & 0xFFFF_FFFF) as usize;
        let msg_flags: u16 = ((header >> 8) & 0xFFFF) as u16;
        let flag_compressed = 0 != (msg_flags & NETCONNECTION_MESSAGE_FLAG_COMPRESSED);
        
        //Calculate and check total size
        let message_size = data_length + MESSAGE_HEADER_SIZE;
        if 0 == message_size || message_size > MESSAGE_MAX_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                NetConnectionCodecError::WrongLength(message_size),
            ));
        }

        // Ensure that the buffer has enough space to read the incoming
        // payload  
        src.reserve(data_length.saturating_sub(src.len()));
        
        //Construct header parts
        Ok(Some(HeaderParts {
            data_length,
            flag_compressed,
        }))
    }

    fn decode_data(&self, header: &HeaderParts, src: &mut BytesMut) -> Option<NetConnectionMessage> {
        // At this point, the buffer has already had the required capacity
        // reserved. All there is to do is read.
        if src.len() < header.data_length {
            return None;
        }
        let mut data_len = header.data_length;
        
        //Extract NETID before actual data
        let source_netid = src.get_u64();
        let destination_netid = src.get_u64();
        data_len -= SIZE_OF_NETID * 2;

        //Read the message content and message struct
        Some(NetConnectionMessage {
            source_netid,
            destination_netid,
            compressed: header.flag_compressed,
            data: src.split_to(data_len).freeze(),
        })
    }
}

impl Decoder for NetConnectionCodecDecoder {
    type Item = NetConnectionMessage;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if let None = self.header {
            match self.decode_head(src)? {
                Some(header) => {
                    self.header = Some(header);
                },
                None => return Ok(None),
            }
        }

        if let Some(ref header) = self.header {
            match self.decode_data(header, src) {
                Some(data) => {
                    // Update the decode state
                    self.header = None;

                    // Make sure the buffer has enough space to read the next head
                    src.reserve(MESSAGE_HEADER_SIZE.saturating_sub(src.len()));

                    Ok(Some(data))
                }
                None => Ok(None),
            }
        } else {
            //Not supposed to happen
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                NetConnectionCodecError::UnknownState,
            ));
        }
    }

    fn decode_eof(&mut self, buf: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        match self.decode(buf)? {
            Some(frame) => Ok(Some(frame)),
            None => {
                if buf.is_empty() {
                    Ok(None)
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        NetConnectionCodecError::BytesRemaining(buf.len())
                    ))
                }
            }
        }
    }
}

impl NetConnectionCodecEncoder {
    pub fn new() -> Self {
        Self {}
    }

    #[allow(dead_code)]
    pub fn new_framed<T: AsyncWrite>(inner: T) -> codec::FramedWrite<T, Self> {
        codec::FramedWrite::new(inner, Self::new())
    }
}

impl Encoder<NetConnectionMessage> for NetConnectionCodecEncoder {
    type Error = io::Error;

    fn encode(&mut self, item: NetConnectionMessage, dst: &mut BytesMut) -> Result<(), io::Error> {
        //Get data and total message length
        let mut data_len = item.data.len();
        data_len += SIZE_OF_NETID * 2;
        let total_len = data_len + MESSAGE_HEADER_SIZE;

        //Check msg max size
        if 0 == total_len || total_len > MESSAGE_MAX_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                NetConnectionCodecError::WrongLength(total_len),
            ));
        }

        // Reserve capacity in the destination buffer to fit the frame and
        // length field (plus adjustment).
        dst.reserve(total_len);

        //Set header
        let mut flags: u16 = 0;
        if item.compressed {
            flags |= NETCONNECTION_MESSAGE_FLAG_COMPRESSED;
        }
        let mut header: u64 = MESSAGE_HEADER_MAGIC;
        header |= (flags as u64) << 8;
        header |= (data_len as u64) << 24;
        dst.put_u64(header);
        
        //Add soource and destination NETID
        dst.put_u64(item.source_netid);
        dst.put_u64(item.destination_netid);

        // Write the frame to the buffer
        dst.extend_from_slice(&item.data[..]);

        Ok(())
    }
}

impl Decoder for NetConnectionCodec {
    type Item = <NetConnectionCodecDecoder as Decoder>::Item;
    type Error = <NetConnectionCodecDecoder as Decoder>::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        self.decoder.decode(src)
    }

    fn decode_eof(&mut self, buf: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        self.decoder.decode_eof(buf)
    }
}

impl Encoder<NetConnectionMessage> for NetConnectionCodec {
    type Error = <NetConnectionCodecEncoder as Encoder<NetConnectionMessage>>::Error;

    fn encode(&mut self, item: NetConnectionMessage, dst: &mut BytesMut) -> Result<(), Self::Error> {
        self.encoder.encode(item, dst)
    }
}

impl fmt::Display for NetConnectionCodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NetConnectionCodecError::HeaderMagic(header) => {
                write!(f, "Message header magic error {:X}", header)
            },
            NetConnectionCodecError::WrongLength(length) => {
                write!(f, "Message length error {}", length)
            },
            NetConnectionCodecError::BytesRemaining(length) => {
                write!(f, "Stream bytes remaining after EOF {}", length)
            },
            NetConnectionCodecError::UnknownState => {
                write!(f, "Unknown state")
            },
        }
    }
}

impl std::error::Error for NetConnectionCodecError {}

