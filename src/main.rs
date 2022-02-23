use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use filter::SpamHistory;
use influxdb::{InfluxDbWriteable, WriteQuery};
use reqwest::header::HeaderValue;
use tokio::sync::RwLock;

use futures::stream::StreamExt;

use tracing::Instrument;

use twilight_cache_inmemory::{InMemoryCache, ResourceType};
use twilight_embed_builder::{EmbedBuilder, EmbedFieldBuilder};
use twilight_gateway::Event;
use twilight_gateway::Shard;
use twilight_http::request::prelude::RequestReactionType;
use twilight_http::Client as HttpClient;
use twilight_mention::Mention;
use twilight_model::application::interaction::Interaction;
use twilight_model::channel::{Message, Reaction, ReactionType};
use twilight_model::gateway::Intents;
use twilight_model::gateway::payload::incoming::MessageUpdate;
use twilight_model::id::{CommandId, GuildId};

use color_eyre::eyre::Result;

use config::*;
use model::MessageInfo;

mod action;
mod command;
mod config;
mod confusable;
mod filter;
mod model;

const DEFAULT_RELOAD_INTERVAL: u64 = 5 * 60;

#[derive(Clone, Debug)]
struct State {
    cfg: Arc<Config>,
    guild_cfgs: Arc<RwLock<HashMap<GuildId, GuildConfig>>>,
    http: Arc<HttpClient>,
    cache: Arc<InMemoryCache>,
    spam_history: Arc<RwLock<SpamHistory>>,
    cmd_ids: Arc<RwLock<HashMap<GuildId, Option<CommandId>>>>,
    influx_client: Arc<Option<influxdb::Client>>,
    influx_report_count: Arc<AtomicUsize>,
    armed: Arc<AtomicBool>,
}

#[derive(Debug, InfluxDbWriteable)]
struct EventTimingReport {
    time: DateTime<Utc>,
    guild: String,
    channel: String,
    time_taken: f64,
    #[influxdb(tag)]
    action_kind: &'static str,
    #[influxdb(tag)]
    development: bool,
}

#[derive(Debug, InfluxDbWriteable)]
struct MessageFilterReport {
    time: DateTime<Utc>,
    guild: String,
    channel: String,
}

#[derive(Debug, InfluxDbWriteable)]
struct ReactionFilterReport {
    time: DateTime<Utc>,
    guild: String,
    channel: String,
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
    tracing_subscriber::fmt()
        .json()
        .with_thread_ids(true)
        .with_thread_names(true)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
}

async fn send_influx_point(state: &State, point: &WriteQuery) -> Result<()> {
    if let Some(influx_client) = state.influx_client.as_ref() {
        if let Some(influx_cfg) = state.cfg.influx.as_ref() {
            let count = state.influx_report_count.fetch_add(1, Ordering::Relaxed);
            if count % influx_cfg.report_every_n == 0 {
                influx_client.query(point).await?;
            }
        }
    }

    Ok(())
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

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    init_tracing();
    dotenv::dotenv().ok();

    filter::init_globals();

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

    let _sentry_guard = if let Some(sentry_config) = &cfg.sentry {
        Some(sentry::init((
            sentry_config.url.clone(),
            sentry::ClientOptions {
                release: sentry::release_name!(),
                ..Default::default()
            },
        )))
    } else {
        None
    };

    let influx_client = if let Some(influx_cfg) = &cfg.influx {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Token {}", &influx_cfg.token)).unwrap(),
        );
        let reqwest_client = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .unwrap();
        let influx_client = influxdb::Client::new(&influx_cfg.url, &influx_cfg.database)
            .with_http_client(reqwest_client);
        Some(influx_client)
    } else {
        None
    };

    let intents =
        Intents::GUILD_MESSAGES | Intents::GUILD_MEMBERS | Intents::GUILD_MESSAGE_REACTIONS;

    let (shard, mut events) = Shard::builder(discord_token.clone(), intents).build();
    shard.start().await?;

    let http = Arc::new(HttpClient::new(discord_token));
    let cache = InMemoryCache::builder()
        .resource_types(ResourceType::MESSAGE | ResourceType::MEMBER | ResourceType::USER)
        .build();

    let cfg = Arc::new(cfg);
    let spam_history = Arc::new(RwLock::new(filter::SpamHistory::new()));
    let initial_guild_configs =
        config::load_guild_configs(&cfg.guild_config_dir, &cfg.active_guilds)
            .map_err(|(_, e)| e)?;
    let cmd_ids = HashMap::new();

    let state = State {
        armed: Arc::new(AtomicBool::new(cfg.armed_by_default)),
        http,
        spam_history,
        cfg,
        cache: Arc::new(cache),
        guild_cfgs: Arc::new(RwLock::new(initial_guild_configs)),
        cmd_ids: Arc::new(RwLock::new(cmd_ids)),
        influx_client: Arc::new(influx_client),
        influx_report_count: Arc::new(AtomicUsize::new(0)),
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
            Some(event) = events.next() => {
                state.cache.update(&event);
                tokio::spawn(handle_event_wrapper(event, state.clone()).instrument(tracing::debug_span!("Handling event")));
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
}

async fn handle_event_wrapper(event: Event, state: State) {
    let start = Instant::now();
    let result = handle_event(&event, state.clone()).await;
    let end = Instant::now();
    let time = end - start;

    if let Err(report) = result {
        tracing::error!(result = %report, event = ?event, "Error handling event");
    }

    let (guild_id, channel_id, action_kind) = match event {
        Event::MessageCreate(message) => {
            let message = message.0;

            (
                message.guild_id.unwrap(),
                message.channel_id,
                "message create",
            )
        }
        Event::MessageUpdate(update) => (
            update.guild_id.unwrap(),
            update.channel_id,
            "message update",
        ),
        Event::ReactionAdd(rxn) => {
            let rxn = rxn.0;
            (rxn.guild_id.unwrap(), rxn.channel_id, "reaction")
        }
        _ => return,
    };

    #[cfg(debug_assertions)]
    let development = true;
    #[cfg(not(debug_assertions))]
    let development = false;

    let report = EventTimingReport {
        time: Utc::now(),
        time_taken: time.as_secs_f64(),
        guild: guild_id.to_string(),
        channel: channel_id.to_string(),
        action_kind,
        development,
    };

    let result = send_influx_point(&state, &report.into_query("event_report")).await;
    if let Err(err) = result {
        tracing::error!("Unable to send Influx report: {:?}", err);
    }
}

#[tracing::instrument("Handling event")]
async fn handle_event(event: &Event, state: State) -> Result<()> {
    match event {
        Event::MessageCreate(message) => {
            let message = &message.0;
            filter_message(message, state).await?;
        }
        Event::MessageUpdate(update) => {
            filter_message_edit(update, &state).await?;
        }
        Event::ReactionAdd(rxn) => {
            let rxn = &rxn.0;
            filter_reaction(rxn, state).await?;
        }
        Event::Ready(ready) => {
            state.http.set_application_id(ready.application.id);
            let guild_cfgs = state.guild_cfgs.read().await;
            let mut cmd_ids = state.cmd_ids.write().await;

            for (guild_id, guild_config) in guild_cfgs.iter() {
                let cmd_id = command::update_guild_commands(
                    &state.http,
                    *guild_id,
                    None,
                    guild_config.slash_commands.as_ref(),
                    None,
                )
                .await?;
                cmd_ids.insert(*guild_id, cmd_id);
            }
        }
        Event::InteractionCreate(interaction) => {
            let interaction = &interaction.0;
            match interaction {
                Interaction::ApplicationCommand(cmd) => {
                    command::handle_command(state.clone(), cmd.as_ref()).await?;
                }
                _ => {}
            }
        }
        _ => {}
    }

    Ok(())
}

#[tracing::instrument("Reloading guild configurations")]
async fn reload_guild_configs(state: &State) -> Result<(), (GuildId, eyre::Report)> {
    tracing::debug!("Reloading guild configurations");
    let new_guild_configs =
        crate::config::load_guild_configs(&state.cfg.guild_config_dir, &state.cfg.active_guilds)?;
    let mut command_ids = state.cmd_ids.write().await;
    let mut guild_cfgs = state.guild_cfgs.write().await;

    // We can't interact with commands until we have an application ID from the
    // gateway. Don't try if we don't have one yet.
    if state.http.application_id().is_some() {
        for (guild_id, new_guild_config) in &new_guild_configs {
            tracing::trace!(%guild_id, "Updating guild commands");
            // We should always have an old guild config when this method is invoked,
            // because we load configs initially before entering the event loop.
            let old_guild_config = guild_cfgs.get(guild_id).expect("No old guild config?");
            let command_id = command_ids.get(guild_id).map(|v| *v).unwrap_or(None);

            let new_command_id = command::update_guild_commands(
                &state.http,
                *guild_id,
                old_guild_config.slash_commands.as_ref(),
                new_guild_config.slash_commands.as_ref(),
                command_id,
            )
            .await
            .map_err(|e| (*guild_id, e))?;
            command_ids.insert(*guild_id, new_command_id);
        }
    }

    *guild_cfgs = new_guild_configs;

    Ok(())
}

#[tracing::instrument("Filtering message")]
async fn filter_message_info(
    guild_id: GuildId,
    message_info: &MessageInfo<'_>,
    state: &State,
    context: &str,
) -> Result<()> {
    let guild_cfgs = state.guild_cfgs.read().await;
    if let Some(guild_config) = guild_cfgs.get(&guild_id) {
        if message_info.author_is_bot && !guild_config.include_bots {
            tracing::trace!(?guild_id, author = %message_info.author_id, "Skipping message filtration because message was sent by a bot and include_bots is false for this guild");
            return Ok(());
        }

        tracing::trace!(?message_info, "Filtering message");

        if let Some(message_filters) = &guild_config.messages {
            let (mut filter_result, mut actions, mut filter_name) = (None, None, None);

            for filter in message_filters {
                let scoping = filter
                    .scoping
                    .as_ref()
                    .or(guild_config.default_scoping.as_ref());
                if let Some(scoping) = scoping {
                    if !scoping.is_included(message_info.channel_id, message_info.author_roles) {
                        continue;
                    }
                }

                tracing::trace!(?filter.rules, "Filtering message against a filter");

                let test_result = filter.filter_message(&message_info);
                if test_result.is_err() {
                    filter_result = Some(test_result);
                    actions = filter.actions.as_ref();
                    filter_name = Some(filter.name.as_str());
                    break;
                }
            }

            if filter_result.is_none() {
                if let Some(spam_config) = &guild_config.spam {
                    let scoping = spam_config
                        .scoping
                        .as_ref()
                        .or(guild_config.default_scoping.as_ref());
                    let is_in_scope = scoping
                        .map(|s| s.is_included(message_info.channel_id, message_info.author_roles))
                        .unwrap_or(false);

                    if is_in_scope {
                        let now = (Utc::now().timestamp_millis() as u64) * 1000;
                        filter_result = Some(
                            filter::check_spam_record(
                                &message_info,
                                &spam_config,
                                state.spam_history.clone(),
                                now,
                            )
                            .await,
                        );
                        actions = spam_config.actions.as_ref();
                        filter_name = Some("Spam")
                    }
                }
            }

            if let Some(Err(reason)) = filter_result {
                if let Some(actions) = actions {
                    let mut deleted = false;

                    for action in actions {
                        match action {
                            MessageFilterAction::Delete => {
                                if deleted {
                                    continue;
                                }

                                tracing::debug!(
                                    %message_info.id,
                                    %message_info.channel_id,
                                    %message_info.author_id,
                                    %reason,
                                    "Deleting message");

                                if !state.armed.load(Ordering::Relaxed) {
                                    tracing::debug!(%message_info.id, %message_info.channel_id, %message_info.author_id, %reason, "Aborting: Chrysanthemum has not been armed.");
                                    continue;
                                }

                                let result = state
                                    .http
                                    .delete_message(message_info.channel_id, message_info.id)
                                    .exec()
                                    .await;

                                if let Err(err) = result {
                                    tracing::error!(
                                        %message_info.id,
                                        %message_info.channel_id,
                                        %message_info.author_id,
                                        ?err,
                                        "Error deleting message");
                                }

                                deleted = true;
                            }
                            MessageFilterAction::SendLog { channel_id } => {
                                tracing::debug!(
                                    %message_info.id,
                                    %message_info.channel_id,
                                    %message_info.author_id,
                                    %channel_id,
                                    "Sending message filtration log message"
                                );

                                let description = if message_info.content.len() > 0 {
                                    format!("```{}```", message_info.content)
                                } else {
                                    "<no content>".to_string()
                                };

                                let result = state
                                    .http
                                    .create_message(*channel_id)
                                    .embeds(&[EmbedBuilder::new()
                                        .title("Message filtered")
                                        .field(EmbedFieldBuilder::new(
                                            "Filter",
                                            filter_name.unwrap(),
                                        ))
                                        .field(
                                            EmbedFieldBuilder::new(
                                                "Author",
                                                format!(
                                                    "<@{}>",
                                                    message_info.author_id.to_string()
                                                ),
                                            )
                                            .build(),
                                        )
                                        .field(
                                            EmbedFieldBuilder::new(
                                                "Channel",
                                                message_info.channel_id.mention().to_string(),
                                            )
                                            .build(),
                                        )
                                        .field(
                                            EmbedFieldBuilder::new("Reason", reason.clone())
                                                .build(),
                                        )
                                        .field(EmbedFieldBuilder::new("Context", context).build())
                                        .description(description)
                                        .build()
                                        .unwrap()])
                                    .unwrap()
                                    .exec()
                                    .await;

                                if let Err(err) = result {
                                    tracing::error!(
                                        %message_info.id,
                                        %message_info.channel_id,
                                        %message_info.author_id,
                                        %channel_id,
                                        ?err,
                                        "Error sending log message"
                                    );
                                }
                            }
                            MessageFilterAction::SendMessage {
                                channel_id,
                                content,
                                requires_armed,
                            } => {
                                if *requires_armed && !state.armed.load(Ordering::Relaxed) {
                                    continue;
                                }

                                let formatted_content = content
                                    .replace("$USER_ID", &message_info.author_id.to_string());
                                let formatted_content =
                                    formatted_content.replace("$FILTER_REASON", &reason);

                                const MAX_CHARS: usize = 2_000;
                                const MESSAGE_PREVIEW: &'static str = "$MESSAGE_PREVIEW";
                                const ELLIPSIS: &'static str = "…";

                                let formatted_content = if formatted_content.contains(MESSAGE_PREVIEW) {
                                    let available_length = MAX_CHARS - formatted_content.len() - MESSAGE_PREVIEW.len();
                                    let truncated_content = if message_info.content.len() > available_length {
                                        let mut last_index = available_length - ELLIPSIS.len();
                                        while !message_info.content.is_char_boundary(last_index) {
                                            last_index -= 1;
                                        }

                                        Cow::Owned(format!("{}{}", &message_info.content[0..last_index], ELLIPSIS))
                                    } else {
                                        Cow::Borrowed(message_info.content)
                                    };

                                    debug_assert!(truncated_content.len() <= available_length);
                                    formatted_content.replacen(MESSAGE_PREVIEW, &truncated_content, 1)
                                } else {
                                    formatted_content
                                };

                                let result = state
                                    .http
                                    .create_message(*channel_id)
                                    .content(&formatted_content)
                                    .unwrap()
                                    .exec()
                                    .await;
                                if let Err(err) = result {
                                    tracing::error!(
                                        %message_info.id,
                                        %message_info.channel_id,
                                        %message_info.author_id,
                                        %channel_id,
                                        %formatted_content,
                                        ?err,
                                        "Error sending message"
                                    );
                                }
                            }
                        }
                    }
                }

                let report = MessageFilterReport {
                    time: Utc::now(),
                    guild: guild_id.to_string(),
                    channel: message_info.channel_id.to_string(),
                };

                send_influx_point(&state, &report.into_query(context)).await?;
            }
        }
    }

    Ok(())
}

#[tracing::instrument("Filtering message")]
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

            return Ok(())
        }
    };

    let message_info = MessageInfo {
        id: message.id,
        author_id: message.author.id,
        channel_id: message.channel_id,
        timestamp: message.timestamp,
        author_is_bot: message.author.bot,
        author_roles: &member.roles,
        content: &message.content,
        attachments: &message.attachments,
        stickers: &message.sticker_items,
    };

    filter_message_info(guild_id, &message_info, &state, "message create").await
}

#[tracing::instrument("Filtering reaction")]
async fn filter_reaction(rxn: &Reaction, state: State) -> Result<()> {
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
            for filter in reaction_filters {
                let scoping = filter
                    .scoping
                    .as_ref()
                    .or(guild_config.default_scoping.as_ref());
                if let Some(scoping) = scoping {
                    if !scoping.is_included(rxn.channel_id, &member.roles) {
                        continue;
                    }
                }

                let filter_result = filter.filter_reaction(&rxn.emoji);

                if let Err(reason) = filter_result {
                    let actions = filter
                        .actions
                        .as_ref()
                        .or(guild_config.default_actions.as_ref());
                    if let Some(actions) = actions {
                        let request_emoji = match &rxn.emoji {
                            twilight_model::channel::ReactionType::Custom { id, name, .. } => {
                                RequestReactionType::Custom {
                                    id: *id,
                                    name: name.as_deref(),
                                }
                            }
                            twilight_model::channel::ReactionType::Unicode { name } => {
                                RequestReactionType::Unicode { name: &name }
                            }
                        };

                        for action in actions {
                            match action {
                                MessageFilterAction::Delete => {
                                    tracing::debug!(
                                        reaction.channel = %rxn.channel_id,
                                        reaction.message = %rxn.message_id,
                                        reaction.emoji = ?rxn.emoji,
                                        reaction.author = %member.user.id,
                                        "Deleting reactions on message");

                                    if !state.armed.load(Ordering::Relaxed) {
                                        tracing::debug!(
                                            reaction.channel = %rxn.channel_id,
                                            reaction.message = %rxn.message_id,
                                            reaction.emoji = ?rxn.emoji,
                                            reaction.author = %member.user.id,
                                            "Aborting: Chrysanthemum has not been armed.");

                                        continue;
                                    }

                                    let result = state
                                        .http
                                        .delete_all_reaction(
                                            rxn.channel_id,
                                            rxn.message_id,
                                            &request_emoji,
                                        )
                                        .exec()
                                        .await;
                                    if let Err(err) = result {
                                        tracing::error!(
                                            reaction.channel = %rxn.channel_id,
                                            reaction.message = %rxn.message_id,
                                            reaction.emoji = ?rxn.emoji,
                                            reaction.author = %member.user.id,
                                            error = ?err,
                                            "Error deleting reactions on message"
                                        );
                                    }
                                }
                                MessageFilterAction::SendLog { channel_id } => {
                                    let rxn_string = match &rxn.emoji {
                                        ReactionType::Custom { id, .. } => id.mention().to_string(),
                                        ReactionType::Unicode { name } => name.clone(),
                                    };

                                    tracing::debug!(
                                        reaction.channel = %rxn.channel_id,
                                        reaction.message = %rxn.message_id,
                                        reaction.emoji = ?rxn.emoji,
                                        reaction.author = %member.user.id,
                                        target_channel = %channel_id,
                                        "Sending emoji filtration message");

                                    let result = state
                                        .http
                                        .create_message(*channel_id)
                                        .embeds(&[EmbedBuilder::new()
                                            .title("Reaction filtered")
                                            .field(EmbedFieldBuilder::new("Filter", &filter.name))
                                            .field(EmbedFieldBuilder::new(
                                                "Author",
                                                format!("<@{}>", member.user.id.to_string()),
                                            ))
                                            .field(EmbedFieldBuilder::new(
                                                "Channel",
                                                rxn.channel_id.mention().to_string(),
                                            ))
                                            .field(EmbedFieldBuilder::new("Reason", reason.clone()))
                                            .field(EmbedFieldBuilder::new("Reaction", rxn_string))
                                            .build()
                                            .unwrap()])
                                        .unwrap()
                                        .exec()
                                        .await;

                                    if let Err(err) = result {
                                        tracing::error!(
                                            reaction.channel = %rxn.channel_id,
                                            reaction.message = %rxn.message_id,
                                            reaction.emoji = ?rxn.emoji,
                                            reaction.author = %member.user.id,
                                            target_channel = %channel_id,
                                            error = ?err,
                                            "Error sending message to channel"
                                        );
                                    }
                                }
                                MessageFilterAction::SendMessage {
                                    channel_id,
                                    content,
                                    requires_armed,
                                } => {
                                    if *requires_armed && !state.armed.load(Ordering::Relaxed) {
                                        continue;
                                    }

                                    let formatted_content =
                                        content.replace("$USER_ID", &member.user.id.to_string());
                                    let formatted_content =
                                        formatted_content.replace("$FILTER_REASON", &reason);

                                    let result = state
                                        .http
                                        .create_message(*channel_id)
                                        .content(&formatted_content)
                                        .unwrap()
                                        .exec()
                                        .await;
                                    if let Err(err) = result {
                                        tracing::error!(
                                            reaction.channel = %rxn.channel_id,
                                            reaction.message = %rxn.message_id,
                                            reaction.emoji = ?rxn.emoji,
                                            reaction.author = %member.user.id,
                                            %channel_id,
                                            %formatted_content,
                                            ?err,
                                            "Error sending message"
                                        );
                                    }
                                }
                            }

                            let report = ReactionFilterReport {
                                time: Utc::now(),
                                guild: guild_id.to_string(),
                                channel: rxn.channel_id.to_string(),
                            };

                            send_influx_point(&state, &report.into_query("reaction_filter"))
                                .await?;
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

#[tracing::instrument("Filtering message edit via HTTP request")]
async fn filter_message_edit_http(update: &MessageUpdate, state: &State) -> Result<()> {
    let guild_id = match update.guild_id {
        Some(id) => id,
        None => return Ok(()),
    };

    let http_message = state
        .http
        .message(update.channel_id, update.id)
        .exec()
        .await?
        .model()
        .await?;
    
    let cached_member = state.cache.member(guild_id, http_message.author.id);
    // Exists purely as a lifetime extension.
    let http_member_roles;
    let author_roles = match cached_member.as_ref() {
        Some(member) => member.roles(),
        None => {
            http_member_roles = Some(state.http.guild_member(guild_id, http_message.author.id).exec().await?.model().await?.roles.iter().map(|r| *r).collect::<Vec<_>>());
            http_member_roles.as_ref().map(|r| &r[..]).unwrap()
        },
    };

    let message_info = MessageInfo {
        id: http_message.id,
        author_id: http_message.author.id,
        channel_id: http_message.channel_id,
        timestamp: http_message.timestamp,
        author_is_bot: http_message.author.bot,
        author_roles: author_roles,
        content: &http_message.content,
        attachments: &http_message.attachments,
        stickers: &http_message.sticker_items,
    };

    filter_message_info(guild_id, &message_info, &state, "message edit").await
}

#[tracing::instrument("Filtering message edit")]
async fn filter_message_edit(update: &MessageUpdate, state: &State) -> Result<()> {
    let guild_id = match update.guild_id {
        Some(id) => id,
        None => return Ok(()),
    };

    let cached_message = state.cache.message(update.id);

    match (cached_message, update.content.as_ref()) {
        (Some(message), Some(content)) => {
            tracing::trace!("Got message from cache and content from update");

            let (author_id, author_is_bot) = match update.author.as_ref() {
                Some(author) => (author.id, author.bot),
                None => {
                    let cached_author = state.cache.user(message.author());
                    match cached_author {
                        Some(author) => (author.id, author.bot),
                        None => return filter_message_edit_http(update, state).await,
                    }
                }
            };

            let cached_member = state.cache.member(guild_id, message.author());
            let author_roles = match cached_member.as_ref() {
                Some(member) => member.roles(),
                None => return filter_message_edit_http(update, state).await,
            };

            let message_info = MessageInfo {
                id: update.id,
                author_id,
                author_is_bot,
                author_roles,
                content: &content,
                channel_id: message.channel_id(),
                timestamp: message.timestamp(),
                attachments: message.attachments(),
                stickers: message.sticker_items(),
            };

            filter_message_info(guild_id, &message_info, state, "message edit").await
        }
        _ => filter_message_edit_http(update, state).await,
    }
}

async fn send_notification_to_guild(
    state: &State,
    guild_id: GuildId,
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
                .embeds(&[builder.build()?])?
                .exec()
                .await?;
        }
    }

    Ok(())
}
