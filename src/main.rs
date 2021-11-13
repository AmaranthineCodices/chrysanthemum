use std::collections::HashMap;
use std::error::Error;
use std::sync::{Arc, Mutex};

use filter::SpamHistory;
use regex::Regex;
use tokio::sync::RwLock;

use futures::stream::StreamExt;

use twilight_cache_inmemory::{InMemoryCache, ResourceType};
use twilight_embed_builder::{EmbedBuilder, EmbedFieldBuilder};
use twilight_gateway::Shard;
use twilight_gateway::Event;
use twilight_http::Client as HttpClient;
use twilight_http::request::prelude::RequestReactionType;
use twilight_mention::Mention;
use twilight_model::channel::{Reaction, ReactionType};
use twilight_model::gateway::Intents;

use config::*;

mod config;
mod filter;

#[derive(Debug)]
struct GuildStats {
    filtered_messages: u64,
    filtered_reactions: u64,
    filtered_usernames: u64,
}

type Stats = HashMap<twilight_model::id::GuildId, Arc<Mutex<GuildStats>>>;

#[derive(Clone)]
struct State {
    stats: Arc<RwLock<Stats>>,
    cfg: Arc<Config>,
    http: Arc<HttpClient>,
    spam_history: Arc<RwLock<SpamHistory>>,
}

#[tokio::main]
async fn main() {
    dotenv::dotenv().ok();
    pretty_env_logger::init();

    filter::init_globals();

    let discord_token =
        std::env::var("DISCORD_TOKEN").expect("Couldn't retrieve DISCORD_TOKEN variable");

    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "chrysanthemum.cfg.json".to_owned());

    // Ugly: Strip out single-line comments from the source. serde_json doesn't
    // support comments, but config files kind of need them.
    let comment_regex = Regex::new("//[^\n]*\n").unwrap();
    let cfg_str = std::fs::read_to_string(&config_path).expect("couldn't read config file");
    let cfg_json = comment_regex.replace_all(&cfg_str, "");
    let cfg: Config = serde_json::from_str(&cfg_json).expect("Couldn't deserialize config");
    let cfg_validate_result = config::validate_config(&cfg);
    if cfg_validate_result.is_err() {
        let errs = cfg_validate_result.unwrap_err();
        log::error!("Configuration errors were encountered: {:#?}", errs);
        return;
    }

    let intents =
        Intents::GUILD_MESSAGES | Intents::GUILD_MEMBERS | Intents::GUILD_MESSAGE_REACTIONS;

    let (shard, mut events) = Shard::builder(discord_token.clone(), intents).build();
    shard.start().await.unwrap();

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
        http,
        spam_history,
        cfg,
        stats,
    };

    while let Some(event) = events.next().await {
        cache.update(&event);

        tokio::spawn(handle_event(event, state.clone()));
    }
}

async fn handle_event(event: Event, state: State) -> Result<(), Box<dyn Error + Send + Sync>> {
    match event {
        Event::MessageCreate(message) => {
            let message = message.0;
            // guild_id will always be set in this case, because we
            // will only ever receive guild messages via our intent.
            if let Some(guild_config) = state.cfg.guilds.get(&message.guild_id.unwrap()) {
                if message.author.bot && !guild_config.include_bots
                {
                    return Ok(());
                }

                if let Some(message_filters) = &guild_config.messages {
                    let (mut filter_result, mut actions) = (None, None);

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

                        let test_result = filter.filter_message(&message);
                        if test_result.is_err() {
                            filter_result = Some(test_result);
                            actions = filter.actions.as_ref();
                            break;
                        }
                    }

                    if filter_result.is_none() {
                        if let Some(spam_config) = &guild_config.spam {
                            filter_result = Some(
                                filter::check_spam_record(
                                    &message,
                                    &spam_config,
                                    state.spam_history.clone(),
                                )
                                .await,
                            );
                            actions = spam_config.actions.as_ref();
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

                                        state.http.delete_message(message.channel_id, message.id).exec().await.unwrap();
                                        deleted = true;
                                    },
                                    MessageFilterAction::SendMessage { channel_id, .. } => {
                                        state.http.create_message(*channel_id).embeds(&[
                                            EmbedBuilder::new()
                                                .title("Message filtered")
                                                .field(EmbedFieldBuilder::new("Author", format!("<@{}>", message.author.id.to_string())))
                                                .field(EmbedFieldBuilder::new("Reason", reason.clone()))
                                                .field(EmbedFieldBuilder::new("Text", format!("```{}```", message.content)))
                                                .build().unwrap()
                                        ]).unwrap().exec().await.unwrap();
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
        },
        Event::ReactionAdd(rxn) => {
            let rxn = rxn.0;

            if rxn.guild_id.is_none() {
                return Ok(());
            }

            let guild_id = rxn.guild_id.unwrap();

            if rxn.member.is_none() {
                return Ok(());
            }

            let member = rxn.member.unwrap();

            if let Some(guild_config) = state.cfg.guilds.get(&guild_id) {
                if member.user.bot && !guild_config.include_bots {
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
                                    twilight_model::channel::ReactionType::Custom { id, name, .. } => RequestReactionType::Custom {
                                        id: *id, name: name.as_deref(),
                                    },
                                    twilight_model::channel::ReactionType::Unicode { name } => RequestReactionType::Unicode { name: &name },
                                };

                                for action in actions {
                                    match action {
                                        MessageFilterAction::Delete => {
                                            state.http.delete_all_reaction(rxn.channel_id, rxn.message_id, &request_emoji).exec().await?;
                                        }
                                        MessageFilterAction::SendMessage {
                                            channel_id,
                                            ..
                                        } => {
                                            let rxn_string = match &rxn.emoji {
                                                ReactionType::Custom { id, .. } => id.mention().to_string(),
                                                ReactionType::Unicode { name } => name.clone(),
                                            };
                            
                                            state.http.create_message(*channel_id).embeds(&[
                                                EmbedBuilder::new()
                                                    .title("Reaction filtered")
                                                    .field(EmbedFieldBuilder::new("Author", format!("<@{}>", member.user.id.to_string())))
                                                    .field(EmbedFieldBuilder::new("Reason", reason.clone()))
                                                    .field(EmbedFieldBuilder::new("Reaction", rxn_string))
                                                    .build().unwrap()
                                            ]).unwrap().exec().await.unwrap();
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
        }
        _ => {},
    }

    Ok(())
}
