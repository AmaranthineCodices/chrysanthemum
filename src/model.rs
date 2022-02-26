use twilight_model::{
    channel::{message::sticker::MessageSticker, Attachment, ReactionType},
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
    pub(crate) author_id: UserId,
    pub(crate) message_id: MessageId,
    pub(crate) channel_id: ChannelId,
    pub(crate) reaction: ReactionType,
}

#[cfg(test)]
pub(crate) mod test {
    use twilight_model::{id::{MessageId, UserId, ChannelId, EmojiId}, datetime::Timestamp, channel::ReactionType};

    use super::{MessageInfo, ReactionInfo};

    // const Option::unwrap is not stabilized yet.
    // Use unsafe to skip the check for 0.
    pub(crate) const MESSAGE_ID: MessageId = unsafe { MessageId::new_unchecked(1) };
    pub(crate) const CHANNEL_ID: ChannelId = unsafe { ChannelId::new_unchecked(2) };
    pub(crate) const USER_ID: UserId = unsafe { UserId::new_unchecked(3) };
    pub(crate) const GOOD_CONTENT: &'static str = "this is an okay message https://discord.gg/ discord.gg/roblox";
    pub(crate) const BAD_CONTENT: &'static str = "asdf bad message z̷̢͈͓̥̤͕̰̤̔͒̄̂̒͋̔̀̒͑̈̅̍̐a̶̡̘̬̯̩̣̪̤̹̖͓͉̿l̷̼̬͊͊̀́̽̑̕g̵̝̗͇͇̈́̄͌̈́͊̌̋͋̑̌̕͘͘ơ̵̢̰̱̟͑̀̂͗́̈́̀  https://example.com/ discord.gg/evilserver";

    pub(crate) fn message(content: &'static str) -> MessageInfo<'static> {
        MessageInfo {
            author_is_bot: false,
            id: MESSAGE_ID,
            author_id: USER_ID,
            channel_id: CHANNEL_ID,
            author_roles: &[],
            content: content,
            timestamp: Timestamp::from_secs(100).unwrap(),
            attachments: &[],
            stickers: &[],
        }
    }

    pub(crate) fn message_at_time(content: &'static str, timestamp: u64) -> MessageInfo<'static> {
        let mut info = message(content);
        info.timestamp = Timestamp::from_secs(timestamp).unwrap();
        info
    }

    pub(crate) fn default_reaction(rxn: &'static str) -> ReactionInfo<'static> {
        ReactionInfo {
            author_is_bot: false,
            author_roles: &[],
            author_id: USER_ID,
            channel_id: CHANNEL_ID,
            message_id: MESSAGE_ID,
            reaction: ReactionType::Unicode {
                name: rxn.to_string(),
            }
        }
    }
}
