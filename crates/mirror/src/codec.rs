//! The tokio codec: `Framed<_, Codec<In, Out>>` reads `In` and writes `Out`. The daemon uses
//! `Codec<ClientMsg, DaemonMsg>`; an async client uses `Codec<DaemonMsg, ClientMsg>`.

use std::marker::PhantomData;

use bytes::{Buf, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

use crate::frame::{WireError, split_frame};
use crate::msg::Message;

#[derive(Debug)]
pub struct Codec<In, Out> {
    _types: PhantomData<fn(Out) -> In>,
}

impl<In, Out> Codec<In, Out> {
    pub fn new() -> Self {
        Codec {
            _types: PhantomData,
        }
    }
}

impl<In, Out> Default for Codec<In, Out> {
    fn default() -> Self {
        Self::new()
    }
}

impl<In: Message, Out> Decoder for Codec<In, Out> {
    type Item = In;
    type Error = WireError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<In>, WireError> {
        let Some((frame, used)) = split_frame(src)? else {
            return Ok(None);
        };
        let msg = In::decode(frame);
        src.advance(used);
        msg.map(Some)
    }
}

impl<In, Out: Message> Encoder<Out> for Codec<In, Out> {
    type Error = WireError;

    fn encode(&mut self, item: Out, dst: &mut BytesMut) -> Result<(), WireError> {
        dst.extend_from_slice(&item.to_bytes());
        Ok(())
    }
}
