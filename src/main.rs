use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::{DateTime, Utc};
use filter::SpamHistory;
use influxdb::{InfluxDbWriteable, WriteQuery};
use reqwest::header::HeaderValue;
use tokio::sync::RwLock;

use futures::stream::StreamExt;

use tracing::Instrument;

use twilight_cache_inmemory::{InMemoryCache, ResourceType};
use twilight_embed_builder::{EmbedBuilder, EmbedFieldBuilder};
use twilight_gateway::Shard;
use twilight_gateway::Event;
use twilight_http::Client as HttpClient;
use twilight_http::request::prelude::RequestReactionType;
use twilight_mention::Mention;
use twilight_model::application::interaction::Interaction;
use twilight_model::channel::ReactionType;
use twilight_model::gateway::Intents;

use color_eyre::eyre::Result;

use config::*;

mod command;
mod config;
mod confusable;
mod filter;

#[derive(Debug)]
struct GuildStats {
    filtered_messages: u64,
    filtered_reactions: u64,
    filtered_usernames: u64,
}

type Stats = HashMap<twilight_model::id::GuildId, Arc<Mutex<GuildStats>>>;

#[derive(Clone, Debug)]
struct State {
    stats: Arc<RwLock<Stats>>,
    cfg: Arc<Config>,
    http: Arc<HttpClient>,
    spam_history: Arc<RwLock<SpamHistory>>,
    cmd_state: Arc<RwLock<Option<command::CommandState>>>,
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
    #[influxdb(tag)] action_kind: &'static str,
    #[influxdb(tag)] development: bool,
}

#[cfg(debug_assertions)]
fn init_tracing() {
    tracing_subscriber::fmt()
        .pretty()
        .with_thread_ids(true)
        .with_thread_names(true)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("chrysanthemum=debug".parse().unwrap())
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

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    init_tracing();
    dotenv::dotenv().ok();

    filter::init_globals();

    let discord_token =
        std::env::var("DISCORD_TOKEN")?;

    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "chrysanthemum.cfg.json".to_owned());

    let cfg_json = std::fs::read_to_string(&config_path).expect("couldn't read config file");
    let cfg: Config = serde_json::from_str(&cfg_json).expect("Couldn't deserialize config");
    let cfg_validate_result = config::validate_config(&cfg);
    if cfg_validate_result.is_err() {
        let errs = cfg_validate_result.unwrap_err();
        tracing::error!("Configuration errors were encountered: {:#?}", errs);
        return Err(eyre::eyre!("{:#?}", errs));
    }

    let _sentry_guard = if let Some(sentry_config) = &cfg.sentry {
        Some(sentry::init((sentry_config.url.clone(), sentry::ClientOptions {
            release: sentry::release_name!(),
            ..Default::default()
        })))
    } else {
        None
    };

    let influx_client = if let Some(influx_cfg) = &cfg.influx {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("Authorization", HeaderValue::from_str(&format!("Token {}", &influx_cfg.token)).unwrap());
        let reqwest_client = reqwest::Client::builder().default_headers(headers).build().unwrap();
        let influx_client = influxdb::Client::new(&influx_cfg.url, &influx_cfg.database).with_http_client(reqwest_client);
        Some(influx_client)
    } else {
        None
    };

    let intents =
        Intents::GUILD_MESSAGES | Intents::GUILD_MEMBERS | Intents::GUILD_MESSAGE_REACTIONS;

    let (shard, mut events) = Shard::builder(discord_token.clone(), intents).build();
    shard.start().await?;

    let http = Arc::new(HttpClient::new(discord_token));
    let cache = InMemoryCache::builder().resource_types(ResourceType::MESSAGE).build();

    let cfg = Arc::new(cfg);
    let spam_history = Arc::new(RwLock::new(filter::SpamHistory::new()));
    let stats = Arc::new(RwLock::new(Stats::new()));

    {
        let mut stats = stats.write().await;
        for (guild_id, _) in &cfg.guilds {
            stats.insert(
                *guild_id,
                Arc::new(Mutex::new(GuildStats {
                    filtered_messages: 0,
                    filtered_reactions: 0,
                    filtered_usernames: 0,
                })),
            );
        }
    }

    let state = State {
        armed: Arc::new(AtomicBool::new(cfg.armed_by_default)),
        http,
        spam_history,
        cfg,
        stats,
        cmd_state: Arc::new(RwLock::new(None)),
        influx_client: Arc::new(influx_client),
        influx_report_count: Arc::new(AtomicUsize::new(0)),
    };

    tracing::info!("About to enter main event loop; Chrysanthemum is now online.");

    while let Some(event) = events.next().await {
        cache.update(&event);
        tokio::spawn(handle_event_wrapper(event, state.clone()).instrument(tracing::debug_span!("Handling event")));
    }

    Ok(())
}

async fn handle_event_wrapper(event: Event, state: State) {
    let start = Instant::now();
    handle_event(&event, state.clone()).await;
    let end = Instant::now();
    let time = end - start;

    let (guild_id, channel_id, action_kind) = match event {
        Event::MessageCreate(message) => {
            let message = message.0;
            
            (message.guild_id.unwrap(), message.channel_id, "message create")
        },
        Event::ReactionAdd(rxn) => {
            let rxn = rxn.0;
            (rxn.guild_id.unwrap(), rxn.channel_id, "reaction")
        },
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

async fn handle_event(event: &Event, state: State) {
    match event {
        Event::MessageCreate(message) => {
            let message = &message.0;
            let span = tracing::debug_span!("Filtering message", %message.content);

            async move {
                // guild_id will always be set in this case, because we
                // will only ever receive guild messages via our intent.
                if let Some(guild_config) = state.cfg.guilds.get(&message.guild_id.unwrap()) {
                    if message.author.bot && !guild_config.include_bots
                    {
                        tracing::trace!(?message.guild_id, author = %message.author.id, "Skipping message filtration because message was sent by a bot and include_bots is false for this guild");
                        return;
                    }
    
                    if let Some(message_filters) = &guild_config.messages {
                        let (mut filter_result, mut actions, mut filter_name) = (None, None, None);
    
                        for filter in message_filters {
                            let scoping = filter
                                .scoping
                                .as_ref()
                                .or(guild_config.default_scoping.as_ref());
                            if let Some(scoping) = scoping {
                                if !scoping.is_included(
                                    message.channel_id,
                                    &message.member.as_ref().unwrap().roles,
                                ) {
                                    continue;
                                }
                            }
    
                            tracing::trace!(?filter.rules, "Filtering message against a filter");
    
                            let test_result = filter.filter_message(&message);
                            if test_result.is_err() {
                                filter_result = Some(test_result);
                                actions = filter.actions.as_ref();
                                filter_name = Some(filter.name.as_str());
                                break;
                            }
                        }
    
                        if filter_result.is_none() {
                            if let Some(spam_config) = &guild_config.spam {
                                let spam_span = tracing::trace_span!("Running spam checks");
                                let _enter = spam_span.enter();
    
                                filter_result = Some(
                                    filter::check_spam_record(
                                        &message,
                                        &spam_config,
                                        state.spam_history.clone(),
                                    )
                                    .await,
                                );
                                actions = spam_config.actions.as_ref();
                                filter_name = Some("Spam")
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
                                                %message.id,
                                                %message.channel_id,
                                                %message.author.id,
                                                %reason,
                                                "Deleting message");
                                            
                                            if !state.armed.load(Ordering::Relaxed) {
                                                tracing::debug!(%message.id, %message.channel_id, %message.author.id, %reason, "Aborting: Chrysanthemum has not been armed.");
                                                continue;
                                            }

                                            let result = state.http.delete_message(message.channel_id, message.id).exec().await;
    
                                            if let Err(err) = result {
                                                tracing::error!(
                                                    %message.id,
                                                    %message.channel_id,
                                                    %message.author.id,
                                                    ?err,
                                                    "Error deleting message");
                                            }
    
                                            deleted = true;
                                        },
                                        MessageFilterAction::SendLog { channel_id } => {
                                            tracing::debug!(
                                                %message.id,
                                                %message.channel_id,
                                                %message.author.id,
                                                %channel_id,
                                                "Sending message filtration log message"
                                            );

                                            let description = if message.content.len() > 0 {
                                                format!("```{}```", message.content)
                                            } else {
                                                "<no content>".to_string()
                                            };
    
                                            let result = state.http.create_message(*channel_id).embeds(&[
                                                EmbedBuilder::new()
                                                    .title("Message filtered")
                                                    .field(EmbedFieldBuilder::new("Filter", filter_name.unwrap()))
                                                    .field(EmbedFieldBuilder::new("Author", format!("<@{}>", message.author.id.to_string())).build())
                                                    .field(EmbedFieldBuilder::new("Channel", message.channel_id.mention().to_string()).build())
                                                    .field(EmbedFieldBuilder::new("Reason", reason.clone()).build())
                                                    .description(description)
                                                    .build().unwrap()
                                            ]).unwrap().exec().await;
    
                                            if let Err(err) = result {
                                                tracing::error!(
                                                    %message.id,
                                                    %message.channel_id,
                                                    %message.author.id,
                                                    %channel_id,
                                                    ?err,
                                                    "Error sending log message"
                                                );
                                            }
                                        },
                                        MessageFilterAction::SendMessage { channel_id, content, requires_armed } => {
                                            if *requires_armed && !state.armed.load(Ordering::Relaxed) {
                                                continue;
                                            }

                                            let formatted_content = content.replace("$USER_ID", &message.author.id.to_string());
                                            let formatted_content = formatted_content.replace("$FILTER_REASON", &reason);

                                            let result = state.http.create_message(*channel_id).content(&formatted_content).unwrap().exec().await;
                                            if let Err(err) = result {
                                                tracing::error!(
                                                    %message.id,
                                                    %message.channel_id,
                                                    %message.author.id,
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
    
                            let stats = state.stats.read().await;
                            let mut guild_stats =
                                stats[&message.guild_id.unwrap()].lock().unwrap();
                            guild_stats.filtered_messages =
                                guild_stats.filtered_messages.saturating_add(1);
                        }
                    }
                }
            }.instrument(span).await;
        },
        Event::ReactionAdd(rxn) => {
            let rxn = &rxn.0;
            let span = tracing::debug_span!("Filtering reaction", reaction.emoji = ?rxn.emoji);

            async move {
                if rxn.guild_id.is_none() {
                    tracing::trace!("A reaction was added, but no guild ID is present. Ignoring.");
                    return;
                }
    
                let guild_id = rxn.guild_id.unwrap();
    
                if rxn.member.is_none() {
                    tracing::trace!("A reaction was added, but no member information is present. Ignoring.");
                    return;
                }
    
                let member = rxn.member.as_ref().unwrap();
    
                if let Some(guild_config) = state.cfg.guilds.get(&guild_id) {
                    if member.user.bot && !guild_config.include_bots {
                        return;
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
                                        twilight_model::channel::ReactionType::Custom { id, name, .. } => RequestReactionType::Custom {
                                            id: *id, name: name.as_deref(),
                                        },
                                        twilight_model::channel::ReactionType::Unicode { name } => RequestReactionType::Unicode { name: &name },
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

                                                let result = state.http.delete_all_reaction(rxn.channel_id, rxn.message_id, &request_emoji).exec().await;
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
                                            MessageFilterAction::SendLog {
                                                channel_id,
                                            } => {
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
                                
                                                let result = state.http.create_message(*channel_id).embeds(&[
                                                    EmbedBuilder::new()
                                                        .title("Reaction filtered")
                                                        .field(EmbedFieldBuilder::new("Filter", &filter.name))
                                                        .field(EmbedFieldBuilder::new("Author", format!("<@{}>", member.user.id.to_string())))
                                                        .field(EmbedFieldBuilder::new("Channel", rxn.channel_id.mention().to_string()))
                                                        .field(EmbedFieldBuilder::new("Reason", reason.clone()))
                                                        .field(EmbedFieldBuilder::new("Reaction", rxn_string))
                                                        .build().unwrap()
                                                ]).unwrap().exec().await;
                                                
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
                                            },
                                            MessageFilterAction::SendMessage { channel_id, content, requires_armed } => {
                                                if *requires_armed && !state.armed.load(Ordering::Relaxed) {
                                                    continue;
                                                }

                                                let formatted_content = content.replace("$USER_ID", &member.user.id.to_string());
                                                let formatted_content = formatted_content.replace("$FILTER_REASON", &reason);

                                                let result = state.http.create_message(*channel_id).content(&formatted_content).unwrap().exec().await;
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
                                    }
                                }
    
                                let stats = state.stats.read().await;
                                let mut guild_stats = stats[&guild_id].lock().unwrap();
                                guild_stats.filtered_reactions =
                                    guild_stats.filtered_reactions.saturating_add(1);
                            }
                        }
                    }
                }
            }.instrument(span).await;
        },
        Event::Ready(ready) => {
            state.http.set_application_id(ready.application.id);
            let command_state = command::create_commands(state.clone()).await;

            if let Err(err) = command_state {
                tracing::error!(?err, "Unable to create slash commands");
            }
            else
            {
                let command_state = command_state.unwrap();
                let mut state_command_state = state.cmd_state.write().await;
                *state_command_state = Some(command_state);
            }
        },
        Event::InteractionCreate(interaction) => {
            let interaction = &interaction.0;
            match interaction {
                Interaction::ApplicationCommand(cmd) => {
                    command::handle_command(state.clone(), cmd.as_ref()).await;
                },
                _ => {},
            }
        }
        _ => {},
    }
}
