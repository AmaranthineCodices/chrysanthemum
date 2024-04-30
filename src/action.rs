use twilight_http::{
    request::{channel::reaction::RequestReactionType, AuditLogReason},
    Client,
};
use twilight_mention::Mention;
use twilight_model::{
    channel::message::ReactionType,
    id::{
        marker::{ChannelMarker, GuildMarker, MessageMarker, UserMarker},
        Id,
    },
    util::Timestamp,
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
    Ban {
        user_id: Id<UserMarker>,
        guild_id: Option<Id<GuildMarker>>,
        delete_message_seconds: u32,
        reason: String,
    },
    Kick {
        user_id: Id<UserMarker>,
        guild_id: Option<Id<GuildMarker>>,
        reason: String,
    },
    Timeout {
        user_id: Id<UserMarker>,
        guild_id: Option<Id<GuildMarker>>,
        reason: String,
        duration: i64,
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
                http.delete_message(*channel_id, *message_id).await?;
            }
            Self::SendMessage { to, content, .. } => {
                http.create_message(*to).content(content)?.await?;
            }
            Self::Ban {
                user_id,
                guild_id,
                delete_message_seconds,
                reason,
            } => {
                if let Some(guild_id) = guild_id {
                    http.create_ban(*guild_id, *user_id)
                        .delete_message_seconds(*delete_message_seconds)?
                        .reason(reason)?
                        .await?;
                }
            }
            Self::Kick {
                user_id,
                guild_id,
                reason,
            } => {
                if let Some(guild_id) = guild_id {
                    http.remove_guild_member(*guild_id, *user_id)
                        .reason(reason)?
                        .await?;
                }
            }
            Self::Timeout {
                user_id,
                guild_id,
                duration,
                reason,
            } => {
                if let Some(guild_id) = guild_id {
                    let timeout_expires_at =
                        Timestamp::from_secs(chrono::Utc::now().timestamp() + *duration)?;

                    http.update_guild_member(*guild_id, *user_id)
                        .communication_disabled_until(Some(timeout_expires_at))?
                        .reason(reason)?
                        .await?;
                }
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

                if !content.is_empty() {
                    embed_builder = embed_builder.description(format!("```{}```", content));
                }

                http.create_message(*to)
                    .embeds(&[embed_builder.build()])
                    .unwrap()
                    .await?;
            }
        };

        Ok(())
    }

    pub(crate) fn requires_armed(&self) -> bool {
        match self {
            MessageAction::Delete { .. } => true,
            MessageAction::Ban { .. } => true,
            MessageAction::Kick { .. } => true,
            MessageAction::Timeout { .. } => true,
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
    Ban {
        user_id: Id<UserMarker>,
        guild_id: Option<Id<GuildMarker>>,
        delete_message_seconds: u32,
        reason: String,
    },
    Kick {
        user_id: Id<UserMarker>,
        guild_id: Option<Id<GuildMarker>>,
        reason: String,
    },
    Timeout {
        user_id: Id<UserMarker>,
        guild_id: Option<Id<GuildMarker>>,
        reason: String,
        duration: i64,
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
                    ReactionType::Unicode { name } => RequestReactionType::Unicode { name },
                };

                http.delete_all_reaction(*channel_id, *message_id, &request_emoji)
                    .await?;
            }
            Self::SendMessage { to, content, .. } => {
                http.create_message(*to).content(content)?.await?;
            }
            Self::Ban {
                user_id,
                guild_id,
                delete_message_seconds,
                reason,
            } => {
                if let Some(guild_id) = guild_id {
                    http.create_ban(*guild_id, *user_id)
                        .delete_message_seconds(*delete_message_seconds)?
                        .reason(reason)?
                        .await?;
                }
            }
            Self::Kick {
                user_id,
                guild_id,
                reason,
            } => {
                if let Some(guild_id) = guild_id {
                    http.remove_guild_member(*guild_id, *user_id)
                        .reason(reason)?
                        .await?;
                }
            }
            Self::Timeout {
                user_id,
                guild_id,
                duration,
                reason,
            } => {
                if let Some(guild_id) = guild_id {
                    let timeout_expires_at =
                        Timestamp::from_secs(chrono::Utc::now().timestamp() + *duration)?;

                    http.update_guild_member(*guild_id, *user_id)
                        .communication_disabled_until(Some(timeout_expires_at))?
                        .reason(reason)?
                        .await?;
                }
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
                                format!("https://discordapp.com/{}/{}", channel, message),
                            )
                            .build(),
                        )
                        .field(EmbedFieldBuilder::new("Reason", filter_reason).build())
                        .field(EmbedFieldBuilder::new("Reaction", rxn_string).build())
                        .build()])
                    .unwrap()
                    .await?;
            }
        };

        Ok(())
    }

    pub(crate) fn requires_armed(&self) -> bool {
        match self {
            ReactionAction::Delete { .. } => true,
            ReactionAction::Ban { .. } => true,
            ReactionAction::Kick { .. } => true,
            ReactionAction::Timeout { .. } => true,
            ReactionAction::SendMessage { requires_armed, .. } => *requires_armed,
            _ => false,
        }
    }
}
