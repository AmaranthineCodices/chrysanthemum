use twilight_model::{
    channel::{message::sticker::MessageSticker, Attachment, Reaction},
    datetime::Timestamp,
    id::{ChannelId, MessageId, RoleId, UserId},
};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MessageInfo<'a> {
    pub(crate) author_is_bot: bool,
    pub(crate) id: MessageId,
    pub(crate) author_id: UserId,
    pub(crate) channel_id: ChannelId,
    pub(crate) author_roles: &'a [RoleId],
    pub(crate) content: &'a str,
    pub(crate) timestamp: Timestamp,
    pub(crate) attachments: &'a [Attachment],
    pub(crate) stickers: &'a [MessageSticker],
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ReactionInfo<'a> {
    pub(crate) author_is_bot: bool,
    pub(crate) author_roles: &'a [RoleId],
    pub(crate) message_id: MessageId,
    pub(crate) channel_id: ChannelId,
    pub(crate) reaction: Reaction,
}
