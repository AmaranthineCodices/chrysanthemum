use twilight_embed_builder::{EmbedBuilder, EmbedFieldBuilder};
use twilight_http::{request::prelude::RequestReactionType, Client};
use twilight_mention::Mention;
use twilight_model::{
    channel::ReactionType,
    id::{ChannelId, MessageId, UserId},
};

use eyre::Result;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MessageAction {
    Delete {
        message_id: MessageId,
        channel_id: ChannelId,
    },
    SendMessage {
        to: ChannelId,
        content: String,
    },
    SendLog {
        to: ChannelId,
        filter_name: String,
        message_channel: ChannelId,
        content: String,
        filter_reason: String,
        author: UserId,
        context: String,
    },
}

impl MessageAction {
    pub(crate) async fn execute(&self, http: &Client) -> Result<()> {
        match self {
            Self::Delete {
                message_id,
                channel_id,
            } => {
                http.delete_message(*channel_id, *message_id).exec().await?;
            }
            Self::SendMessage { to, content } => {
                http.create_message(*to).content(content)?.exec().await?;
            }
            Self::SendLog {
                to,
                filter_name,
                message_channel,
                content,
                filter_reason,
                author,
                context,
            } => {
                http.create_message(*to)
                    .embeds(&[EmbedBuilder::new()
                        .title("Message filtered")
                        .field(EmbedFieldBuilder::new("Filter", filter_name))
                        .field(
                            EmbedFieldBuilder::new("Author", author.mention().to_string()).build(),
                        )
                        .field(
                            EmbedFieldBuilder::new(
                                "Channel",
                                message_channel.mention().to_string(),
                            )
                            .build(),
                        )
                        .field(EmbedFieldBuilder::new("Reason", filter_reason).build())
                        .field(EmbedFieldBuilder::new("Context", context).build())
                        .description(content)
                        .build()
                        .unwrap()])
                    .unwrap()
                    .exec()
                    .await?;
            }
        };

        Ok(())
    }
}

pub(crate) enum ReactionAction {
    Delete {
        message_id: MessageId,
        channel_id: ChannelId,
        reaction: ReactionType,
    },
    SendMessage {
        to: ChannelId,
        content: String,
    },
    SendLog {
        to: ChannelId,
        filter_name: String,
        message: MessageId,
        channel: ChannelId,
        filter_reason: String,
        author: UserId,
        reaction: ReactionType,
    },
}

impl ReactionAction {
    pub(crate) async fn execute(&self, http: &Client) -> Result<()> {
        match self {
            Self::Delete {
                message_id,
                channel_id,
                reaction,
            } => {
                let request_emoji = match reaction {
                    ReactionType::Custom { id, name, .. } => RequestReactionType::Custom {
                        id: *id,
                        name: name.as_deref(),
                    },
                    ReactionType::Unicode { name } => RequestReactionType::Unicode { name: &name },
                };

                http.delete_all_reaction(*channel_id, *message_id, &request_emoji)
                    .exec()
                    .await?;
            }
            Self::SendMessage { to, content } => {
                http.create_message(*to).content(content)?.exec().await?;
            }
            Self::SendLog {
                to,
                filter_name,
                message,
                channel,
                filter_reason,
                author,
                reaction,
            } => {
                let rxn_string = match reaction {
                    ReactionType::Custom { id, .. } => id.mention().to_string(),
                    ReactionType::Unicode { name } => name.clone(),
                };

                http.create_message(*to)
                    .embeds(&[EmbedBuilder::new()
                        .title("Reaction filtered")
                        .field(EmbedFieldBuilder::new("Filter", filter_name))
                        .field(
                            EmbedFieldBuilder::new("Author", author.mention().to_string()).build(),
                        )
                        .field(
                            EmbedFieldBuilder::new("Channel", channel.mention().to_string())
                                .build(),
                        )
                        .field(
                            EmbedFieldBuilder::new(
                                "Message",
                                format!(
                                    "https://discordapp.com/{}/{}",
                                    channel.to_string(),
                                    message.to_string()
                                ),
                            )
                            .build(),
                        )
                        .field(EmbedFieldBuilder::new("Reason", filter_reason).build())
                        .field(EmbedFieldBuilder::new("Reaction", rxn_string).build())
                        .build()
                        .unwrap()])
                    .unwrap()
                    .exec()
                    .await?;
            }
        };

        Ok(())
    }
}
