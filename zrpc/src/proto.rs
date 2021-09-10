use super::{ConnectionId, PeerId, TypedEnvelope};
use anyhow::Result;
use async_tungstenite::tungstenite::{Error as WebSocketError, Message as WebSocketMessage};
use futures::{SinkExt as _, StreamExt as _};
use prost::Message;
use std::any::{Any, TypeId};
use std::{
    io,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

include!(concat!(env!("OUT_DIR"), "/zed.messages.rs"));

pub trait EnvelopedMessage: Clone + Sized + Send + Sync + 'static {
    const NAME: &'static str;
    fn into_envelope(
        self,
        id: u32,
        responding_to: Option<u32>,
        original_sender_id: Option<u32>,
    ) -> Envelope;
    fn from_envelope(envelope: Envelope) -> Option<Self>;
}

pub trait EntityMessage: EnvelopedMessage {
    fn remote_entity_id(&self) -> u64;
}

pub trait RequestMessage: EnvelopedMessage {
    type Response: EnvelopedMessage;
}

pub trait AnyTypedEnvelope: 'static + Send + Sync {
    fn payload_type_id(&self) -> TypeId;
    fn payload_type_name(&self) -> &'static str;
    fn as_any(&self) -> &dyn Any;
    fn into_any(self: Box<Self>) -> Box<dyn Any + Send + Sync>;
}

impl<T: EnvelopedMessage> AnyTypedEnvelope for TypedEnvelope<T> {
    fn payload_type_id(&self) -> TypeId {
        TypeId::of::<T>()
    }

    fn payload_type_name(&self) -> &'static str {
        T::NAME
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any + Send + Sync> {
        self
    }
}

macro_rules! messages {
    ($($name:ident),* $(,)?) => {
        pub fn build_typed_envelope(sender_id: ConnectionId, envelope: Envelope) -> Option<Box<dyn AnyTypedEnvelope>> {
            match envelope.payload {
                $(Some(envelope::Payload::$name(payload)) => {
                    Some(Box::new(TypedEnvelope {
                        sender_id,
                        original_sender_id: envelope.original_sender_id.map(PeerId),
                        message_id: envelope.id,
                        payload,
                    }))
                }, )*
                _ => None
            }
        }

        $(
            impl EnvelopedMessage for $name {
                const NAME: &'static str = std::stringify!($name);

                fn into_envelope(
                    self,
                    id: u32,
                    responding_to: Option<u32>,
                    original_sender_id: Option<u32>,
                ) -> Envelope {
                    Envelope {
                        id,
                        responding_to,
                        original_sender_id,
                        payload: Some(envelope::Payload::$name(self)),
                    }
                }

                fn from_envelope(envelope: Envelope) -> Option<Self> {
                    if let Some(envelope::Payload::$name(msg)) = envelope.payload {
                        Some(msg)
                    } else {
                        None
                    }
                }
            }
        )*
    };
}

macro_rules! request_messages {
    ($(($request_name:ident, $response_name:ident)),* $(,)?) => {
        $(impl RequestMessage for $request_name {
            type Response = $response_name;
        })*
    };
}

macro_rules! entity_messages {
    ($id_field:ident, $($name:ident),* $(,)?) => {
        $(impl EntityMessage for $name {
            fn remote_entity_id(&self) -> u64 {
                self.$id_field
            }
        })*
    };
}

messages!(
    Ack,
    AddPeer,
    BufferSaved,
    ChannelMessageSent,
    CloseBuffer,
    CloseWorktree,
    Error,
    GetChannelMessages,
    GetChannelMessagesResponse,
    GetChannels,
    GetChannelsResponse,
    GetUsers,
    GetUsersResponse,
    JoinChannel,
    JoinChannelResponse,
    LeaveChannel,
    OpenBuffer,
    OpenBufferResponse,
    OpenWorktree,
    OpenWorktreeResponse,
    Ping,
    RemovePeer,
    SaveBuffer,
    SendChannelMessage,
    SendChannelMessageResponse,
    ShareWorktree,
    ShareWorktreeResponse,
    UpdateBuffer,
    UpdateWorktree,
);

request_messages!(
    (GetChannels, GetChannelsResponse),
    (GetUsers, GetUsersResponse),
    (JoinChannel, JoinChannelResponse),
    (OpenBuffer, OpenBufferResponse),
    (OpenWorktree, OpenWorktreeResponse),
    (Ping, Ack),
    (SaveBuffer, BufferSaved),
    (UpdateBuffer, Ack),
    (ShareWorktree, ShareWorktreeResponse),
    (SendChannelMessage, SendChannelMessageResponse),
    (GetChannelMessages, GetChannelMessagesResponse),
);

entity_messages!(
    worktree_id,
    AddPeer,
    BufferSaved,
    CloseBuffer,
    CloseWorktree,
    OpenBuffer,
    OpenWorktree,
    RemovePeer,
    SaveBuffer,
    UpdateBuffer,
    UpdateWorktree,
);

entity_messages!(channel_id, ChannelMessageSent);

/// A stream of protobuf messages.
pub struct MessageStream<S> {
    stream: S,
}

impl<S> MessageStream<S> {
    pub fn new(stream: S) -> Self {
        Self { stream }
    }

    pub fn inner_mut(&mut self) -> &mut S {
        &mut self.stream
    }
}

impl<S> MessageStream<S>
where
    S: futures::Sink<WebSocketMessage, Error = WebSocketError> + Unpin,
{
    /// Write a given protobuf message to the stream.
    pub async fn write_message(&mut self, message: &Envelope) -> Result<(), WebSocketError> {
        let mut buffer = Vec::with_capacity(message.encoded_len());
        message
            .encode(&mut buffer)
            .map_err(|err| io::Error::from(err))?;
        self.stream.send(WebSocketMessage::Binary(buffer)).await?;
        Ok(())
    }
}

impl<S> MessageStream<S>
where
    S: futures::Stream<Item = Result<WebSocketMessage, WebSocketError>> + Unpin,
{
    /// Read a protobuf message of the given type from the stream.
    pub async fn read_message(&mut self) -> Result<Envelope, WebSocketError> {
        while let Some(bytes) = self.stream.next().await {
            match bytes? {
                WebSocketMessage::Binary(bytes) => {
                    let envelope = Envelope::decode(bytes.as_slice()).map_err(io::Error::from)?;
                    return Ok(envelope);
                }
                WebSocketMessage::Close(_) => break,
                _ => {}
            }
        }
        Err(WebSocketError::ConnectionClosed)
    }
}

impl Into<SystemTime> for Timestamp {
    fn into(self) -> SystemTime {
        UNIX_EPOCH
            .checked_add(Duration::new(self.seconds, self.nanos))
            .unwrap()
    }
}

impl From<SystemTime> for Timestamp {
    fn from(time: SystemTime) -> Self {
        let duration = time.duration_since(UNIX_EPOCH).unwrap();
        Self {
            seconds: duration.as_secs(),
            nanos: duration.subsec_nanos(),
        }
    }
}
