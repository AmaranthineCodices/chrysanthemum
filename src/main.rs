use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use discordant::gateway::{connect_to_gateway, Event, Intents};
use discordant::http::{Client, CreateMessagePayload};
use discordant::types::{
    Embed, EmbedField, InteractionResponse, InteractionResponseMessageFlags, Snowflake,
};
use regex::Regex;
use tokio::sync::RwLock;

use config::*;

mod action;
mod config;
mod filter;

#[derive(Debug)]
struct GuildStats {
    filtered_messages: u64,
    filtered_reactions: u64,
    filtered_usernames: u64,
}

type Stats = HashMap<Snowflake, Arc<Mutex<GuildStats>>>;

async fn create_slash_commands(app_id: Snowflake, config: &Config, client: &Client) {
    for (guild_id, guild) in &config.guilds {
        if guild.slash_commands.is_none() {
            continue;
        }

        let slash_commands = guild.slash_commands.as_ref().unwrap();

        let payload = discordant::http::CreateApplicationCommandPayload {
            name: "chrysanthemum".to_owned(),
            description: "Interact with Chrysanthemum, a message filtration bot.".to_owned(),
            options: Some(vec![
                discordant::types::ApplicationCommandOption {
                    ty: discordant::types::ApplicationCommandOptionType::Subcommand {
                        options: Some(vec![]),
                    },
                    name: "stats".to_owned(),
                    description: "See stats collected in this guild.".to_owned(),
                    required: None,
                },
                discordant::types::ApplicationCommandOption {
                    ty: discordant::types::ApplicationCommandOptionType::Subcommand {
                        options: Some(vec![discordant::types::ApplicationCommandOption {
                            ty: discordant::types::ApplicationCommandOptionType::String {
                                choices: None,
                            },
                            name: "message".to_owned(),
                            description: "The message to test.".to_owned(),
                            required: Some(true),
                        }]),
                    },
                    name: "test".to_owned(),
                    description: "Test a message to see if it will be filtered.".to_owned(),
                    required: None,
                },
            ]),
            default_permission: false,
        };

        let created_command = client
            .create_guild_application_command(app_id, *guild_id, payload)
            .await
            .unwrap();

        client
            .edit_application_command_permissions(
                app_id,
                *guild_id,
                created_command.id,
                slash_commands
                    .roles
                    .iter()
                    .map(|s| discordant::types::ApplicationCommandPermission {
                        id: *s,
                        ty: discordant::types::ApplicationCommandPermissionType::Role,
                        permission: true,
                    })
                    .collect::<Vec<_>>(),
            )
            .await
            .unwrap();
    }
}

async fn send_notification(config: &Config, client: &Client, notification_content: &str) {
    for (_, guild_cfg) in &config.guilds {
        if let Some(notification_settings) = &guild_cfg.notifications {
            let mut message_content = String::new();

            if let Some(roles) = &notification_settings.ping_roles {
                for role in roles {
                    message_content.push_str(&format!("<@&{}>\n", role));
                }
            }

            message_content.push_str(notification_content);
            client
                .create_message(
                    notification_settings.channel,
                    CreateMessagePayload {
                        content: message_content,
                    },
                )
                .await
                .unwrap();
        }
    }
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

    let client = discordant::http::Client::new(&discord_token);
    let gateway_info = client.get_gateway_info().await.unwrap();

    let intents =
        Intents::GUILD_MESSAGES | Intents::GUILD_MEMBERS | Intents::GUILD_MESSAGE_REACTIONS;

    let client = std::sync::Arc::new(client);
    let cfg = std::sync::Arc::new(cfg);
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

    let mut gateway = connect_to_gateway(&gateway_info.url, discord_token, intents)
        .await
        .expect("Could not connect to gateway");

    send_notification(
        &cfg,
        &client,
        ":chart_with_upwards_trend: Chrysanthemum online",
    )
    .await;

    let running = Arc::new(AtomicBool::new(true));
    // let ctrl_c_running = running.clone();
    // ctrlc::set_handler(move || {
    //     ctrl_c_running.store(false, Ordering::SeqCst);
    // })
    // .expect("Couldn't set Ctrl-C handler");

    loop {
        if !running.load(Ordering::SeqCst) {
            log::debug!("Termination requested, shutting down loop");
            gateway.close().await;
            break;
        }

        let event = gateway.next_event().await;

        match event {
            Ok(event) => {
                match event {
                    Event::MessageCreate(message) => {
                        let cfg = cfg.clone();
                        let client = client.clone();
                        let spam_history = spam_history.clone();
                        let stats = stats.clone();
                        tokio::spawn(async move {
                            // guild_id will always be set in this case, because we
                            // will only ever receive guild messages via our intent.
                            if let Some(guild_config) = cfg.guilds.get(&message.guild_id.unwrap()) {
                                if message.author.bot.unwrap_or(false) && !guild_config.include_bots
                                {
                                    return;
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
                                                    spam_history.clone(),
                                                )
                                                .await,
                                            );
                                            actions = spam_config.actions.as_ref();
                                        }
                                    }

                                    if let Some(Err(reason)) = filter_result {
                                        if let Some(actions) = actions {
                                            for action in actions {
                                                action
                                                    .do_action(&reason, &message, &client)
                                                    .await
                                                    .expect("Couldn't perform action");
                                            }
                                        }

                                        let stats = stats.read().await;
                                        let mut guild_stats =
                                            stats[&message.guild_id.unwrap()].lock().unwrap();
                                        guild_stats.filtered_messages =
                                            guild_stats.filtered_messages.saturating_add(1);
                                    }
                                }
                            }
                        });
                    }
                    Event::Ready(ready) => {
                        let cfg = cfg.clone();
                        let client = client.clone();
                        tokio::spawn(async move {
                            create_slash_commands(ready.application.id, &cfg, &client).await;
                        });
                    }
                    Event::InteractionCreate(interaction) => {
                        let guild_id = interaction.guild_id.unwrap();
                        if let Some(data) = &interaction.data {
                            if let Some(options) = &data.options {
                                if let Some(subcommand) = options.first() {
                                    if subcommand.name == "stats" {
                                        let stats = stats.read().await;
                                        let guild_stats = stats[&guild_id].lock().unwrap();
                                        let response =
                                            InteractionResponse::ChannelMessageWithSource {
                                                tts: false,
                                                content: None,
                                                embeds: Some(vec![Embed {
                                                    title: Some("Chrysanthemum stats".to_owned()),
                                                    fields: Some(vec![
                                                        EmbedField {
                                                            name: "Filtered messages".to_owned(),
                                                            value: guild_stats
                                                                .filtered_messages
                                                                .to_string(),
                                                            inline: Some(false),
                                                        },
                                                        EmbedField {
                                                            name: "Filtered reactions".to_owned(),
                                                            value: guild_stats
                                                                .filtered_reactions
                                                                .to_string(),
                                                            inline: Some(false),
                                                        },
                                                        EmbedField {
                                                            name: "Filtered usernames".to_owned(),
                                                            value: guild_stats
                                                                .filtered_usernames
                                                                .to_string(),
                                                            inline: Some(false),
                                                        },
                                                    ]),
                                                    ..Default::default()
                                                }]),
                                                flags: InteractionResponseMessageFlags::EMPHEMERAL,
                                            };

                                        let client = client.clone();
                                        let interaction_token = interaction.token.clone();
                                        let interaction_id = interaction.id;
                                        tokio::spawn(async move {
                                            client
                                                .send_interaction_response(
                                                    interaction_id,
                                                    &interaction_token,
                                                    &response,
                                                )
                                                .await
                                                .unwrap();
                                        });
                                    } else if subcommand.name == "test" {
                                        let cfg = cfg.clone();
                                        let client = client.clone();
                                        let interaction_token = interaction.token.clone();
                                        let interaction_id = interaction.id;

                                        let text = match &subcommand.value {
                                            discordant::types::InteractionDataOptionValue::Subcommand { options: Some(options) } => {
                                                match options.first() {
                                                    Some(text_option) => match &text_option.value {
                                                        discordant::types::InteractionDataOptionValue::String { value } => Some(value.clone()),
                                                        _ => None,
                                                    },
                                                    _ => None,
                                                }
                                            },
                                            _ => None,
                                        };

                                        if let Some(text) = text {
                                            if let Some(guild_config) = cfg.guilds.get(&guild_id) {
                                                let response = if let Some(message_filters) =
                                                    &guild_config.messages
                                                {
                                                    let result = message_filters
                                                        .iter()
                                                        .map(|f| f.filter_text(&text))
                                                        .find(|r| r.is_err())
                                                        .unwrap_or(Ok(()));
                                                    let (embed_title, embed_color) =
                                                        if result.is_ok() {
                                                            ("Success".to_owned(), 0x00FF00)
                                                        } else {
                                                            ("Failed".to_owned(), 0xFF0000)
                                                        };

                                                    let mut embed_fields = vec![EmbedField {
                                                        name: "Text".to_owned(),
                                                        value: text,
                                                        inline: Some(false),
                                                    }];

                                                    match result {
                                                        Ok(()) => {}
                                                        Err(reason) => {
                                                            embed_fields.push(EmbedField {
                                                                name: "Reason".to_owned(),
                                                                value: reason,
                                                                inline: Some(false),
                                                            })
                                                        }
                                                    }

                                                    InteractionResponse::ChannelMessageWithSource {
                                                        tts: false,
                                                        content: None,
                                                        embeds: Some(vec![
                                                            Embed {
                                                                title: Some(embed_title),
                                                                color: Some(embed_color),
                                                                fields: Some(embed_fields),
                                                                ..Default::default()
                                                            }
                                                        ]),
                                                        flags: InteractionResponseMessageFlags::EMPHEMERAL,
                                                    }
                                                } else {
                                                    InteractionResponse::ChannelMessageWithSource {
                                                        tts: false,
                                                        content: None,
                                                        embeds: Some(vec![Embed {
                                                            title: Some("Error".to_owned()),
                                                            description: Some("This guild is not filtering messages".to_owned()),
                                                            color: Some(0xFF0000),
                                                            ..Default::default()
                                                        }]),
                                                        flags: InteractionResponseMessageFlags::EMPHEMERAL,
                                                    }
                                                };

                                                tokio::spawn(async move {
                                                    client
                                                        .send_interaction_response(
                                                            interaction_id,
                                                            &interaction_token,
                                                            &response,
                                                        )
                                                        .await
                                                        .unwrap();
                                                });
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Event::MessageReactionAdd {
                        guild_id,
                        channel_id,
                        message_id,
                        member,
                        emoji,
                        ..
                    } => {
                        let cfg = cfg.clone();
                        let client = client.clone();
                        let stats = stats.clone();
                        tokio::spawn(async move {
                            if guild_id.is_none() {
                                return;
                            }

                            let guild_id = guild_id.unwrap();

                            if member.is_none() {
                                return;
                            }

                            let member = member.unwrap();

                            if let Some(guild_config) = cfg.guilds.get(&guild_id) {
                                if member.user.bot.unwrap_or(false) && !guild_config.include_bots {
                                    return;
                                }

                                if let Some(reaction_filters) = &guild_config.reactions {
                                    for filter in reaction_filters {
                                        let scoping = filter
                                            .scoping
                                            .as_ref()
                                            .or(guild_config.default_scoping.as_ref());
                                        if let Some(scoping) = scoping {
                                            if !scoping.is_included(channel_id, &member.roles) {
                                                continue;
                                            }
                                        }

                                        let filter_result = filter.filter_reaction(&emoji);

                                        if let Err(reason) = filter_result {
                                            let actions = filter
                                                .actions
                                                .as_ref()
                                                .or(guild_config.default_actions.as_ref());
                                            if let Some(actions) = actions {
                                                for action in actions {
                                                    match action {
                                                        MessageFilterAction::Delete => {
                                                            client
                                                                .delete_reactions_for_emoji(
                                                                    channel_id, message_id, &emoji,
                                                                )
                                                                .await
                                                                .unwrap();
                                                        }
                                                        MessageFilterAction::SendMessage {
                                                            channel_id: target_channel,
                                                            content,
                                                            batch,
                                                        } => {
                                                            let batch = batch.unwrap_or(true);
                                                            let formatted_content =
                                                                content.replace("$REASON", &reason);
                                                            client
                                                                .create_message(
                                                                    *target_channel,
                                                                    CreateMessagePayload {
                                                                        content: formatted_content,
                                                                    },
                                                                )
                                                                .await
                                                                .unwrap();
                                                        }
                                                    }
                                                }
                                            }

                                            let stats = stats.read().await;
                                            let mut guild_stats = stats[&guild_id].lock().unwrap();
                                            guild_stats.filtered_reactions =
                                                guild_stats.filtered_reactions.saturating_add(1);
                                        }
                                    }
                                }
                            }
                        });
                    }
                    _ => {}
                }
            }
            Err(err) => {
                log::error!("Error: {:?}", err);
                break;
            }
        }
    }

    send_notification(
        &cfg,
        &client,
        ":chart_with_downwards_trend: Chrysanthemum offline",
    )
    .await;
}

#[cfg(test)]
mod test {
    use discordant::types::{Attachment, MessageStickerItem, Message};

    use super::*;

    #[test]
    fn filter_words() {
        let rule = MessageFilterRule::Words {
            words: Regex::new("\\b(a|b)\\b").unwrap(),
        };

        assert_eq!(
            rule.filter_message(&Message {
                content: "c".to_owned(),
                ..Default::default()
            }),
            Ok(())
        );

        assert_eq!(
            rule.filter_message(&Message {
                content: "a".to_owned(),
                ..Default::default()
            }),
            Err("contains word a".to_owned())
        );
    }

    #[test]
    fn filter_regex() {
        let rule = MessageFilterRule::Regex {
            regexes: vec![Regex::new("a|b").unwrap()],
        };

        assert_eq!(
            rule.filter_message(&Message {
                content: "c".to_owned(),
                ..Default::default()
            }),
            Ok(())
        );

        assert_eq!(
            rule.filter_message(&Message {
                content: "a".to_owned(),
                ..Default::default()
            }),
            Err("matches regex a|b".to_owned())
        );
    }

    #[test]
    fn filter_zalgo() {
        filter::init_globals();

        let rule = MessageFilterRule::Zalgo;

        assert_eq!(
            rule.filter_message(&Message {
                content: "c".to_owned(),
                ..Default::default()
            }),
            Ok(())
        );

        assert_eq!(
            rule.filter_message(&Message {
                content: "t̸͈͈̒̑͛ê̷͓̜͎s̴̡͍̳͊t̴̪͙́̚".to_owned(),
                ..Default::default()
            }),
            Err("contains zalgo".to_owned())
        );
    }

    #[test]
    fn filter_mime_type() {
        let allow_rule = MessageFilterRule::MimeType {
            mode: FilterMode::AllowList,
            types: vec!["image/png".to_owned()],
            allow_unknown: true,
        };

        let deny_rule = MessageFilterRule::MimeType {
            mode: FilterMode::DenyList,
            types: vec!["image/png".to_owned()],
            allow_unknown: true,
        };

        let png_message = Message {
            attachments: vec![Attachment {
                content_type: Some("image/png".to_owned()),
                ..Default::default()
            }],
            ..Default::default()
        };

        let gif_message = Message {
            attachments: vec![Attachment {
                content_type: Some("image/gif".to_owned()),
                ..Default::default()
            }],
            ..Default::default()
        };

        assert_eq!(allow_rule.filter_message(&png_message), Ok(()));

        assert_eq!(
            allow_rule.filter_message(&gif_message),
            Err("contains unallowed content type image/gif".to_owned())
        );

        assert_eq!(
            deny_rule.filter_message(&png_message),
            Err("contains denied content type image/png".to_owned())
        );

        assert_eq!(deny_rule.filter_message(&gif_message), Ok(()));
    }

    #[test]
    fn filter_unknown_mime_type() {
        let allow_rule = MessageFilterRule::MimeType {
            mode: FilterMode::AllowList,
            types: vec!["image/png".to_owned()],
            allow_unknown: true,
        };

        let deny_rule = MessageFilterRule::MimeType {
            mode: FilterMode::AllowList,
            types: vec!["image/png".to_owned()],
            allow_unknown: false,
        };

        let unknown_message = Message {
            attachments: vec![Attachment {
                content_type: None,
                ..Default::default()
            }],
            ..Default::default()
        };

        assert_eq!(allow_rule.filter_message(&unknown_message), Ok(()));

        assert_eq!(
            deny_rule.filter_message(&unknown_message),
            Err("unknown content type for attachment".to_owned())
        );
    }

    #[test]
    fn filter_invites() {
        filter::init_globals();

        let allow_rule = MessageFilterRule::Invite {
            mode: FilterMode::AllowList,
            invites: vec!["roblox".to_owned()],
        };

        let deny_rule = MessageFilterRule::Invite {
            mode: FilterMode::DenyList,
            invites: vec!["roblox".to_owned()],
        };

        let roblox_message = Message {
            content: "discord.gg/roblox".to_owned(),
            ..Default::default()
        };

        let not_roblox_message = Message {
            content: "discord.gg/asdf".to_owned(),
            ..Default::default()
        };

        assert_eq!(allow_rule.filter_message(&roblox_message), Ok(()));

        assert_eq!(
            allow_rule.filter_message(&not_roblox_message),
            Err("contains unallowed invite asdf".to_owned())
        );

        assert_eq!(
            deny_rule.filter_message(&roblox_message),
            Err("contains denied invite roblox".to_owned())
        );

        assert_eq!(deny_rule.filter_message(&not_roblox_message), Ok(()));
    }

    #[test]
    fn filter_domains() {
        filter::init_globals();

        let allow_rule = MessageFilterRule::Link {
            mode: FilterMode::AllowList,
            domains: vec!["roblox.com".to_owned()],
        };

        let deny_rule = MessageFilterRule::Link {
            mode: FilterMode::DenyList,
            domains: vec!["roblox.com".to_owned()],
        };

        let roblox_message = Message {
            content: "https://roblox.com/".to_owned(),
            ..Default::default()
        };

        let not_roblox_message = Message {
            content: "https://discord.com/".to_owned(),
            ..Default::default()
        };

        assert_eq!(allow_rule.filter_message(&roblox_message), Ok(()));

        assert_eq!(
            allow_rule.filter_message(&not_roblox_message),
            Err("contains unallowed domain discord.com".to_owned())
        );

        assert_eq!(
            deny_rule.filter_message(&roblox_message),
            Err("contains denied domain roblox.com".to_owned())
        );

        assert_eq!(deny_rule.filter_message(&not_roblox_message), Ok(()));
    }

    #[test]
    fn filter_stickers() {
        let allow_rule = MessageFilterRule::StickerId {
            mode: FilterMode::AllowList,
            stickers: vec![Snowflake::new(0)],
        };

        let deny_rule = MessageFilterRule::StickerId {
            mode: FilterMode::DenyList,
            stickers: vec![Snowflake::new(0)],
        };

        let zero_sticker = Message {
            sticker_items: Some(vec![MessageStickerItem {
                id: Snowflake::new(0),
                name: "test".to_owned(),
                format_type: discordant::types::MessageStickerFormat::Png,
            }]),
            ..Default::default()
        };

        let non_zero_sticker = Message {
            sticker_items: Some(vec![MessageStickerItem {
                id: Snowflake::new(1),
                name: "test".to_owned(),
                format_type: discordant::types::MessageStickerFormat::Png,
            }]),
            ..Default::default()
        };

        assert_eq!(allow_rule.filter_message(&zero_sticker), Ok(()));

        assert_eq!(
            allow_rule.filter_message(&non_zero_sticker),
            Err("contains unallowed sticker 1".to_owned())
        );

        assert_eq!(
            deny_rule.filter_message(&zero_sticker),
            Err("contains denied sticker 0".to_owned())
        );

        assert_eq!(deny_rule.filter_message(&non_zero_sticker), Ok(()));
    }
}
