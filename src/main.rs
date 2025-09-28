use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use action::{MessageAction, ReactionAction};
use chrono::Utc;
use filter::SpamHistory;

use twilight_gateway::StreamExt;

use tokio::sync::RwLock;
use tracing::Instrument;

use twilight_cache_inmemory::{InMemoryCache, ResourceType};
use twilight_gateway::{Event, ShardId};
use twilight_gateway::{EventTypeFlags, Shard};
use twilight_http::Client as HttpClient;
use twilight_mention::Mention;
use twilight_model::application::interaction::InteractionData;
use twilight_model::channel::Message;
use twilight_model::gateway::payload::incoming::MessageUpdate;
use twilight_model::gateway::{GatewayReaction, Intents};
use twilight_model::id::marker::ApplicationMarker;
use twilight_model::id::{marker::GuildMarker, Id};

use color_eyre::eyre::Result;

use config::*;
use model::{MessageInfo, ReactionInfo};
use twilight_util::builder::embed::{EmbedBuilder, EmbedFieldBuilder};

mod action;
mod command;
mod config;
mod confusable;
mod filter;
mod message;
mod model;
mod reaction;

const DEFAULT_RELOAD_INTERVAL: u64 = 5 * 60;

#[derive(Clone, Debug)]
struct State {
    cfg: Arc<Config>,
    guild_cfgs: Arc<RwLock<HashMap<Id<GuildMarker>, GuildConfig>>>,
    http: Arc<HttpClient>,
    application_id: Arc<RwLock<Option<Id<ApplicationMarker>>>>,
    cache: Arc<InMemoryCache>,
    spam_history: Arc<RwLock<SpamHistory>>,
    armed: Arc<AtomicBool>,
}

#[cfg(debug_assertions)]
fn init_tracing() {
    tracing_subscriber::fmt()
        .pretty()
        .with_thread_ids(true)
        .with_thread_names(true)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("chrysanthemum=trace".parse().unwrap()),
        )
        .init();
}

#[cfg(not(debug_assertions))]
fn init_tracing() {
    use tracing_subscriber::prelude::*;

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
}

fn validate_configs() -> Result<()> {
    let config_path = PathBuf::from(
        std::env::args()
            .nth(2)
            .expect("Second argument (config path) not passed"),
    );
    config::load_all_guild_configs(&config_path)?;
    println!("All guild configs are valid");
    Ok(())
}

fn main() -> Result<()> {
    color_eyre::install()?;
    init_tracing();
    dotenv::dotenv().ok();

    let validate_config_mode = std::env::args().nth(1) == Some("validate-configs".to_owned());

    if validate_config_mode {
        validate_configs()?;
        return Ok(());
    }

    let discord_token = std::env::var("DISCORD_TOKEN")?;

    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "chrysanthemum.cfg.yml".to_owned());

    let cfg_json = std::fs::read_to_string(&config_path).expect("couldn't read config file");
    let cfg: Config = serde_yaml::from_str(&cfg_json).expect("Couldn't deserialize config");

    let intents = Intents::GUILD_MESSAGES
        | Intents::GUILD_MEMBERS
        | Intents::GUILD_MESSAGE_REACTIONS
        | Intents::MESSAGE_CONTENT;

    tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap().block_on(async {
        let mut shard = Shard::new(ShardId::ONE, discord_token.clone(), intents);

        let http = Arc::new(HttpClient::new(discord_token));
        let cache = InMemoryCache::builder()
            .resource_types(ResourceType::MESSAGE | ResourceType::MEMBER | ResourceType::USER)
            .build();

        let cfg = Arc::new(cfg);
        let spam_history = Arc::new(RwLock::new(filter::SpamHistory::new()));
        let initial_guild_configs =
            config::load_guild_configs(&cfg.guild_config_dir, &cfg.active_guilds)
                .map_err(|(_, e)| e)?;

        let state = State {
            armed: Arc::new(AtomicBool::new(cfg.armed_by_default)),
            http,
            spam_history,
            cfg,
            cache: Arc::new(cache),
            application_id: Arc::new(RwLock::new(None)),
            guild_cfgs: Arc::new(RwLock::new(initial_guild_configs)),
        };

        tracing::info!("About to enter main event loop; Chrysanthemum is now online.");

        for (guild_id, _) in state.guild_cfgs.read().await.iter() {
            let result = send_notification_to_guild(
                &state,
                *guild_id,
                "Chrysanthemum online",
                "Chrysanthemum is now online.",
            )
            .await;
            if let Err(err) = result {
                tracing::error!(?err, %guild_id, "Error sending up notification");
            }
        }

        let mut interval = tokio::time::interval(Duration::from_secs(
            state.cfg.reload_interval.unwrap_or(DEFAULT_RELOAD_INTERVAL),
        ));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                Some(event) = shard.next_event(EventTypeFlags::all()) => {
                    match event {
                        Ok(event) => {
                            state.cache.update(&event);
                            tokio::spawn(handle_event(event, state.clone()).instrument(tracing::debug_span!("Handling event")));
                        },
                        Err(err) => {
                            tracing::warn!(?err, "error receiving event");
                        }
                    }
                },
                _ = interval.tick() => {
                    let result = reload_guild_configs(&state).await;
                    if let Err((guild_id, report)) = result {
                        tracing::error!(?guild_id, ?report, "Error reloading guild configuration");
                        send_notification_to_guild(&state, guild_id, "Configuration reload failed", &format!("Failure reason:\n```{:#?}```\nConfiguration changes have **not** been applied.", report)).await?;
                    }
                }
            }
        }
    })
}

#[tracing::instrument(skip(state, event), fields(kind = ?event.kind()))]
async fn handle_event(event: Event, state: State) -> Result<()> {
    match event {
        Event::MessageCreate(message) => {
            let message = &message.0;
            filter_message(message, state).await?;
        }
        Event::MessageUpdate(update) => {
            filter_message_edit(&update, &state).await?;
        }
        Event::ReactionAdd(rxn) => {
            let rxn = &rxn.0;
            filter_reaction(rxn, state).await?;
        }
        Event::Ready(ready) => {
            {
                *state.application_id.write().await = Some(ready.application.id);
            }

            let interaction_http = state.http.interaction(ready.application.id);
            let guild_cfgs = state.guild_cfgs.read().await;

            for (guild_id, guild_config) in guild_cfgs.iter() {
                command::update_guild_commands(
                    &interaction_http,
                    *guild_id,
                    guild_config.slash_commands.as_ref(),
                )
                .await?;
            }
        }
        Event::InteractionCreate(interaction) => {
            let interaction = &interaction.0;
            if let Some(InteractionData::ApplicationCommand(cmd)) = &interaction.data {
                command::handle_command(state.clone(), interaction, cmd.as_ref()).await?;
            }
        }
        _ => {}
    }

    Ok(())
}

#[tracing::instrument(skip(state))]
async fn reload_guild_configs(state: &State) -> Result<(), (Id<GuildMarker>, eyre::Report)> {
    tracing::debug!("Reloading guild configurations");
    let new_guild_configs =
        crate::config::load_guild_configs(&state.cfg.guild_config_dir, &state.cfg.active_guilds)?;
    let mut guild_cfgs = state.guild_cfgs.write().await;
    let application_id = *state.application_id.read().await;

    // We can't interact with commands until we have an application ID from the
    // gateway. Don't try if we don't have one yet.
    if let Some(application_id) = application_id {
        let interaction_http = state.http.interaction(application_id);

        for (guild_id, new_guild_config) in &new_guild_configs {
            tracing::trace!(%guild_id, "Updating guild commands");

            command::update_guild_commands(
                &interaction_http,
                *guild_id,
                new_guild_config.slash_commands.as_ref(),
            )
            .await
            .map_err(|e| (*guild_id, e))?;
        }
    }

    *guild_cfgs = new_guild_configs;

    Ok(())
}

#[tracing::instrument(skip(guild_id, message_info, state))]
async fn filter_message_info<'msg>(
    guild_id: Id<GuildMarker>,
    message_info: &'msg MessageInfo<'_>,
    state: &'msg State,
    context: &'static str,
) -> Result<()> {
    let guild_cfgs = state.guild_cfgs.read().await;
    if let Some(guild_config) = guild_cfgs.get(&guild_id) {
        if message_info.author_is_bot && !guild_config.include_bots {
            tracing::trace!(?guild_id, author = %message_info.author_id, "Skipping message filtration because message was sent by a bot and include_bots is false for this guild");
            return Ok(());
        }

        if let Some(message_filters) = &guild_config.messages {
            let now = (Utc::now().timestamp_millis() as u64) * 1000;

            let result = crate::message::filter_and_spam_check_message(
                guild_config.spam.as_ref(),
                &message_filters[..],
                guild_config.default_scoping.as_ref(),
                guild_config.default_actions.as_deref(),
                state.spam_history.clone(),
                message_info,
                context,
                now,
            )
            .await;

            if let Err(failure) = result {
                tracing::trace!(%message_info.id, %message_info.channel_id, %message_info.author_id, ?failure, "Message filtered");

                let armed = state.armed.load(Ordering::Relaxed);
                let mut deleted = false;

                for action in failure.actions {
                    tracing::trace!(?action, "Executing action");

                    // We only want to execute Delete actions once per message,
                    // since we'll get a 404 on subsequent requests.
                    if let MessageAction::Delete { .. } = action {
                        if deleted {
                            tracing::trace!("Skipping duplicate delete action");
                            continue;
                        }

                        deleted = true;
                    }

                    if action.requires_armed() && !armed {
                        tracing::trace!("Skipping action execution because we are not armed");
                        continue;
                    }

                    if let Err(action_err) = action.execute(&state.http).await {
                        tracing::warn!(?action_err, "Error executing action");
                    }
                }

                tracing::trace!("Filtration completed, all actions executed");
            }
        }
    }

    Ok(())
}

#[tracing::instrument(skip_all, fields(id = %message.id.get()))]
async fn filter_message(message: &Message, state: State) -> Result<()> {
    let guild_id = match message.guild_id {
        Some(id) => id,
        None => return Ok(()),
    };

    let member = match message.member.as_ref() {
        Some(member) => member,
        None => {
            // For non-bot users, this should always be set.
            if !message.author.bot {
                tracing::error!(?message.id, "No `member` field attached to message");
            }

            return Ok(());
        }
    };

    let clean_message_content = crate::message::clean_mentions(&message.content, &message.mentions);

    let message_info = MessageInfo {
        id: message.id,
        author_id: message.author.id,
        channel_id: message.channel_id,
        // We can assume guild_id exists since the DM intent is disabled
        guild_id: message.guild_id.unwrap(),
        timestamp: message.timestamp,
        author_is_bot: message.author.bot,
        author_roles: &member.roles,
        content: &clean_message_content,
        attachments: &message.attachments,
        stickers: &message.sticker_items,
    };

    filter_message_info(guild_id, &message_info, &state, "message create").await
}

#[tracing::instrument(skip(state))]
async fn filter_reaction(rxn: &GatewayReaction, state: State) -> Result<()> {
    if rxn.guild_id.is_none() {
        tracing::trace!("A reaction was added, but no guild ID is present. Ignoring.");
        return Ok(());
    }

    let guild_id = rxn.guild_id.unwrap();

    if rxn.member.is_none() {
        tracing::trace!("A reaction was added, but no member information is present. Ignoring.");
        return Ok(());
    }

    let member = rxn.member.as_ref().unwrap();

    let guild_cfgs = state.guild_cfgs.read().await;
    if let Some(guild_config) = guild_cfgs.get(&guild_id) {
        if member.user.bot && !guild_config.include_bots {
            tracing::trace!("A reaction was added by a bot and include_bots is not set. Ignoring.");
            return Ok(());
        }

        if let Some(reaction_filters) = &guild_config.reactions {
            let reaction_info = ReactionInfo {
                author_is_bot: member.user.bot,
                author_roles: &member.roles,
                author_id: rxn.user_id,
                channel_id: rxn.channel_id,
                message_id: rxn.message_id,
                // We can assume guild_id exists since the DM intent is disabled
                guild_id: rxn.guild_id.unwrap(),
                reaction: rxn.emoji.clone(),
            };

            let filter_result = crate::reaction::filter_reaction(
                reaction_filters,
                guild_config.default_scoping.as_ref(),
                guild_config.default_actions.as_deref(),
                &reaction_info,
            );

            if let Err(failure) = filter_result {
                let armed = state.armed.load(Ordering::Relaxed);
                let mut deleted = false;

                for action in failure.actions {
                    if matches!(action, ReactionAction::Delete { .. }) {
                        if deleted {
                            continue;
                        }

                        deleted = true;
                    }

                    if action.requires_armed() && !armed {
                        continue;
                    }

                    if let Err(action_err) = action.execute(&state.http).await {
                        tracing::warn!(?action_err, ?action, "Error executing reaction action");
                    }
                }
            }
        }
    }

    Ok(())
}

#[tracing::instrument(skip(state))]
async fn filter_message_edit_http(update: &MessageUpdate, state: &State) -> Result<()> {
    let guild_id = match update.guild_id {
        Some(id) => id,
        None => return Ok(()),
    };

    let (author_id, author_is_bot) = (update.author.id, update.author.bot);
    let http_message = state
        .http
        .message(update.channel_id, update.id)
        .await?
        .model()
        .await?;

    let author_roles = {
        let cached_member = state.cache.member(guild_id, author_id);
        match cached_member.as_ref() {
            Some(member) => member.roles().to_owned(),
            None => state
                .http
                .guild_member(guild_id, author_id)
                .await?
                .model()
                .await?
                .roles
                .clone(),
        }
    };

    let message_info = MessageInfo {
        id: http_message.id,
        channel_id: http_message.channel_id,
        // We can assume guild_id exists since the DM intent is disabled
        guild_id: http_message.guild_id.unwrap(),
        timestamp: http_message.timestamp,
        author_roles: &author_roles[..],
        content: &http_message.content,
        attachments: &http_message.attachments,
        stickers: &http_message.sticker_items,
        author_id,
        author_is_bot,
    };

    filter_message_info(guild_id, &message_info, state, "message edit").await
}

#[tracing::instrument(skip_all, fields(id = %update.id.get()))]
async fn filter_message_edit(update: &MessageUpdate, state: &State) -> Result<()> {
    let guild_id = match update.guild_id {
        Some(id) => id,
        None => return Ok(()),
    };

    let message = &update.0;

    let timestamp = message.timestamp;
    let attachments = message.attachments.to_owned();
    let sticker_items = message.sticker_items.to_owned();

    let author_roles = {
        let cached_member = state.cache.member(guild_id, update.author.id);
        match cached_member.as_ref() {
            Some(member) => member.roles().to_owned(),
            None => return filter_message_edit_http(update, state).await,
        }
    };

    let clean_message_content =
        crate::message::clean_mentions(&message.content, update.mentions.as_ref());

    let message_info = MessageInfo {
        id: update.id,
        author_id: update.author.id,
        author_is_bot: update.author.bot,
        // We can assume guild_id exists since the DM intent is disabled
        guild_id: update.guild_id.unwrap(),
        author_roles: &author_roles[..],
        content: &clean_message_content,
        channel_id: update.channel_id,
        timestamp,
        attachments: &attachments[..],
        stickers: &sticker_items[..],
    };

    filter_message_info(guild_id, &message_info, state, "message edit").await
}

#[tracing::instrument(skip(state))]
async fn send_notification_to_guild(
    state: &State,
    guild_id: Id<GuildMarker>,
    title: &str,
    body: &str,
) -> Result<()> {
    let guild_configs = state.guild_cfgs.read().await;
    if let Some(guild_config) = guild_configs.get(&guild_id) {
        if let Some(notification_config) = &guild_config.notifications {
            let mut builder = EmbedBuilder::new().title(title).description(body);

            if let Some(ping_roles) = &notification_config.ping_roles {
                let mut cc_body = String::new();
                for role in ping_roles {
                    cc_body += &role.mention().to_string();
                    cc_body += " ";
                }

                builder = builder.field(EmbedFieldBuilder::new("CC", cc_body).build());
            }

            state
                .http
                .create_message(notification_config.channel)
                .embeds(&[builder.build()])
                .await?;
        }
    }

    Ok(())
}
