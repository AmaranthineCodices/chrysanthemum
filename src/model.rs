use twilight_model::{
    channel::{message::sticker::MessageSticker, message::ReactionType, Attachment},
    id::{
        marker::{ChannelMarker, GuildMarker, MessageMarker, RoleMarker, UserMarker},
        Id,
    },
    util::datetime::Timestamp,
};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MessageInfo<'a> {
    pub(crate) author_is_bot: bool,
    pub(crate) id: Id<MessageMarker>,
    pub(crate) author_id: Id<UserMarker>,
    pub(crate) channel_id: Id<ChannelMarker>,
    pub(crate) guild_id: Option<Id<GuildMarker>>,
    pub(crate) author_roles: &'a [Id<RoleMarker>],
    pub(crate) content: &'a str,
    pub(crate) timestamp: Timestamp,
    pub(crate) attachments: &'a [Attachment],
    pub(crate) stickers: &'a [MessageSticker],
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ReactionInfo<'a> {
    pub(crate) author_is_bot: bool,
    pub(crate) author_roles: &'a [Id<RoleMarker>],
    pub(crate) author_id: Id<UserMarker>,
    pub(crate) message_id: Id<MessageMarker>,
    pub(crate) channel_id: Id<ChannelMarker>,
    pub(crate) guild_id: Option<Id<GuildMarker>>,
    pub(crate) reaction: ReactionType,
}

#[cfg(test)]
pub(crate) mod test {
    use twilight_model::{
        channel::message::ReactionType,
        id::{
            marker::{ChannelMarker, GuildMarker, MessageMarker, UserMarker},
            Id,
        },
        util::datetime::Timestamp,
    };

    use super::{MessageInfo, ReactionInfo};

    // const Option::unwrap is not stabilized yet.
    // Use unsafe to skip the check for 0.
    pub(crate) const MESSAGE_ID: Id<MessageMarker> = Id::new(1);
    pub(crate) const CHANNEL_ID: Id<ChannelMarker> = Id::new(2);
    pub(crate) const USER_ID: Id<UserMarker> = Id::new(3);
    pub(crate) const GUILD_ID: Id<GuildMarker> = Id::new(4);
    pub(crate) const GOOD_CONTENT: &'static str =
        "this is an okay message https://discord.gg/ discord.gg/roblox";
    pub(crate) const BAD_CONTENT: &'static str =
        "asdf bad message z̷̢͈͓̥̤͕̰̤̔͒̄̂̒͋̔̀̒͑̈̅̍̐a̶̡̘̬̯̩̣̪̤̹̖͓͉̿l̷̼̬͊͊̀́̽̑̕g̵̝̗͇͇̈́̄͌̈́͊̌̋͋̑̌̕͘͘ơ̵̢̰̱̟͑̀̂͗́̈́̀  https://example.com/ discord.gg/evilserver";

    pub(crate) fn message(content: &'static str) -> MessageInfo<'static> {
        MessageInfo {
            author_is_bot: false,
            id: MESSAGE_ID,
            author_id: USER_ID,
            channel_id: CHANNEL_ID,
            guild_id: Some(GUILD_ID),
            author_roles: &[],
            content: content,
            timestamp: Timestamp::from_secs(100).unwrap(),
            attachments: &[],
            stickers: &[],
        }
    }

    pub(crate) fn message_at_time(content: &'static str, timestamp: i64) -> MessageInfo<'static> {
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
            guild_id: Some(GUILD_ID),
            reaction: ReactionType::Unicode {
                name: rxn.to_string(),
            },
        }
    }
}
