use twilight_http::{request::channel::reaction::RequestReactionType, Client};
use twilight_mention::Mention;
use twilight_model::{
    channel::ReactionType,
    id::{
        marker::{ChannelMarker, MessageMarker, UserMarker},
        Id,
    },
};
use twilight_util::builder::embed::{EmbedBuilder, EmbedFieldBuilder};

use eyre::Result;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MessageAction {
    Delete {
        message_id: Id<MessageMarker>,
        channel_id: Id<ChannelMarker>,
    },
    SendMessage {
        to: Id<ChannelMarker>,
        content: String,
        requires_armed: bool,
    },
    SendLog {
        to: Id<ChannelMarker>,
        filter_name: String,
        message_channel: Id<ChannelMarker>,
        content: String,
        filter_reason: String,
        author: Id<UserMarker>,
        context: &'static str,
    },
}

impl MessageAction {
    #[tracing::instrument(skip(http))]
    pub(crate) async fn execute(&self, http: &Client) -> Result<()> {
        match self {
            Self::Delete {
                message_id,
                channel_id,
            } => {
                http.delete_message(*channel_id, *message_id).exec().await?;
            }
            Self::SendMessage { to, content, .. } => {
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
                let mut embed_builder = EmbedBuilder::new()
                    .title("Message filtered")
                    .field(EmbedFieldBuilder::new("Filter", filter_name))
                    .field(EmbedFieldBuilder::new("Author", author.mention().to_string()).build())
                    .field(
                        EmbedFieldBuilder::new("Channel", message_channel.mention().to_string())
                            .build(),
                    )
                    .field(EmbedFieldBuilder::new("Reason", filter_reason).build())
                    .field(EmbedFieldBuilder::new("Context", *context).build());

                if content.len() > 0 {
                    embed_builder = embed_builder.description(format!("```{}```", content));
                }

                http.create_message(*to)
                    .embeds(&[embed_builder.build()])
                    .unwrap()
                    .exec()
                    .await?;
            }
        };

        Ok(())
    }

    pub(crate) fn requires_armed(&self) -> bool {
        match self {
            MessageAction::Delete { .. } => true,
            MessageAction::SendMessage { requires_armed, .. } => *requires_armed,
            _ => false,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReactionAction {
    Delete {
        message_id: Id<MessageMarker>,
        channel_id: Id<ChannelMarker>,
        reaction: ReactionType,
    },
    SendMessage {
        to: Id<ChannelMarker>,
        content: String,
        requires_armed: bool,
    },
    SendLog {
        to: Id<ChannelMarker>,
        filter_name: String,
        message: Id<MessageMarker>,
        channel: Id<ChannelMarker>,
        filter_reason: String,
        author: Id<UserMarker>,
        reaction: ReactionType,
    },
}

impl ReactionAction {
    #[tracing::instrument(skip(http))]
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
            Self::SendMessage { to, content, .. } => {
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
                        .build()])
                    .unwrap()
                    .exec()
                    .await?;
            }
        };

        Ok(())
    }

    pub(crate) fn requires_armed(&self) -> bool {
        match self {
            &ReactionAction::Delete { .. } => true,
            &ReactionAction::SendMessage { requires_armed, .. } => requires_armed,
            _ => false,
        }
    }
}
