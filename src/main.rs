use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use discordant::gateway::{connect_to_gateway, Event, Intents};
use discordant::http::{Client, CreateMessagePayload, DiscordHttpError};
use discordant::types::{Message, Snowflake};
use once_cell::sync::OnceCell;
use regex::Regex;
use time::OffsetDateTime;
use tokio::sync::RwLock;

use config::*;

mod config;

static ZALGO_REGEX: OnceCell<Regex> = OnceCell::new();
static INVITE_REGEX: OnceCell<Regex> = OnceCell::new();
static LINK_REGEX: OnceCell<Regex> = OnceCell::new();
static SPOILER_REGEX: OnceCell<Regex> = OnceCell::new();
static EMOJI_REGEX: OnceCell<Regex> = OnceCell::new();
static CUSTOM_EMOJI_REGEX: OnceCell<Regex> = OnceCell::new();

impl Action {
    async fn do_action(
        &self,
        fail_reason: &str,
        message: &Message,
        client: &Client,
    ) -> Result<(), DiscordHttpError> {
        match self {
            Action::Delete => client.delete_message(message.channel_id, message.id).await,
            Action::SendMessage {
                channel_id,
                content,
            } => {
                let formatted_content = content.clone();
                let formatted_content =
                    formatted_content.replace("$USER_ID", &message.author.id.to_string());
                let formatted_content = formatted_content.replace("$REASON", fail_reason);
                // Do MESSAGE_CONTENT replacing last, to avoid a situation where
                // we replace part of the message content with another template
                // variable.
                let formatted_content =
                    formatted_content.replace("$MESSAGE_CONTENT", &message.content);
                client
                    .create_message(
                        *channel_id,
                        CreateMessagePayload {
                            content: formatted_content,
                        },
                    )
                    .await
                    .map(|_m| ())
            }
        }
    }
}

type FilterResult = Result<(), String>;

fn filter_values<T, V, I>(
    mode: &FilterMode,
    context: &str,
    values: &mut I,
    filter_values: &[V],
) -> FilterResult
where
    T: std::fmt::Display,
    V: PartialEq<T>,
    I: Iterator<Item = T>,
{
    let result = match mode {
        FilterMode::AllowList => values
            // Note: We use iter().any() instead of contains because we
            // sometimes pass Vec<String> as filter_values, where T is &str -
            // contains isn't smart enough to handle this case.
            .find(|v| !filter_values.iter().any(|f| f == v))
            .map(|v| Err(format!("contains unallowed {} {}", context, v))),
        FilterMode::DenyList => values
            .find(|v| filter_values.iter().any(|f| f == v))
            .map(|v| Err(format!("contains denied {} {}", context, v))),
    };

    result.unwrap_or(Ok(()))
}

impl Scoping {
    fn is_included(&self, channel: Snowflake, author_roles: &Vec<Snowflake>) -> bool {
        if self.include_channels.is_some() {
            if self
                .include_channels
                .as_ref()
                .unwrap()
                .iter()
                .all(|c| *c != channel)
            {
                return false;
            }
        }

        if self.exclude_channels.is_some() {
            if self
                .exclude_channels
                .as_ref()
                .unwrap()
                .iter()
                .any(|c| *c == channel)
            {
                return false;
            }
        }

        if self.exclude_roles.is_some() {
            for excluded_role in self.exclude_roles.as_ref().unwrap() {
                if author_roles.contains(excluded_role) {
                    return false;
                }
            }
        }

        return true;
    }
}

impl MessageFilter {
    fn filter_message(&self, message: &Message) -> FilterResult {
        self.rules
            .iter()
            .map(|f| f.check_match(&message))
            .find(|r| r.is_err())
            .unwrap_or(Ok(()))
    }
}

impl config::MessageFilterRule {
    fn check_match(&self, message: &Message) -> FilterResult {
        match self {
            MessageFilterRule::Words { words } => {
                if let Some(captures) = words.captures(&message.content) {
                    Err(format!(
                        "contains word {}",
                        captures.get(1).unwrap().as_str()
                    ))
                } else {
                    Ok(())
                }
            }
            MessageFilterRule::Regex { regexes } => {
                for regex in regexes {
                    if regex.is_match(&message.content) {
                        return Err(format!("matches regex {}", regex));
                    }
                }

                Ok(())
            }
            MessageFilterRule::Zalgo => {
                let zalgo_regex = ZALGO_REGEX.get().unwrap();
                if zalgo_regex.is_match(&message.content) {
                    Err("contains zalgo".to_owned())
                } else {
                    Ok(())
                }
            }
            MessageFilterRule::MimeType {
                mode,
                types,
                allow_unknown,
            } => {
                if message.attachments.iter().any(|a| a.content_type.is_none()) && !allow_unknown {
                    return Err("unknown content type for attachment".to_owned());
                }

                let mut attachment_types = message
                    .attachments
                    .iter()
                    .filter_map(|a| a.content_type.as_deref());
                filter_values(mode, "content type", &mut attachment_types, types)
            }
            MessageFilterRule::Invite { mode, invites } => {
                let invite_regex = INVITE_REGEX.get().unwrap();
                let mut invite_ids = invite_regex
                    .captures_iter(&message.content)
                    .map(|c| c.get(1).unwrap().as_str());
                filter_values(mode, "invite", &mut invite_ids, invites)
            }
            MessageFilterRule::Link { mode, domains } => {
                let link_regex = LINK_REGEX.get().unwrap();
                let mut link_domains = link_regex
                    .captures_iter(&message.content)
                    .map(|c| c.get(1).unwrap().as_str());
                filter_values(mode, "domain", &mut link_domains, domains)
            }
            MessageFilterRule::StickerId { mode, stickers } => {
                if let Some(message_stickers) = &message.sticker_items {
                    filter_values(
                        mode,
                        "sticker",
                        &mut message_stickers.iter().map(|s| s.id),
                        stickers,
                    )
                } else {
                    Ok(())
                }
            }
            MessageFilterRule::StickerName { stickers } => {
                if let Some(message_stickers) = &message.sticker_items {
                    for sticker in message_stickers {
                        let substring_match = stickers.captures_iter(&sticker.name).nth(0);
                        if let Some(substring_match) = substring_match {
                            return Err(format!(
                                "contains sticker with denied name substring {}",
                                substring_match.get(0).unwrap().as_str()
                            ));
                        }
                    }
                }

                Ok(())
            }
            MessageFilterRule::EmojiName { names } => {
                for capture in CUSTOM_EMOJI_REGEX
                    .get()
                    .unwrap()
                    .captures_iter(&message.content)
                {
                    let name = capture.get(1).unwrap().as_str();
                    let substring_match = names.captures(name);
                    if let Some(substring_match) = substring_match {
                        return Err(format!(
                            "contains emoji with denied name substring {}",
                            substring_match.get(0).unwrap().as_str()
                        ));
                    }
                }

                Ok(())
            }
        }
    }
}

#[derive(Debug)]
struct SpamRecord {
    content: String,
    emoji: u8,
    links: u8,
    attachments: u8,
    spoilers: u8,
    sent_at: OffsetDateTime,
}

impl SpamRecord {
    fn from_message(message: &Message) -> SpamRecord {
        let spoilers = SPOILER_REGEX
            .get()
            .unwrap()
            .find_iter(&message.content)
            .count();
        let emoji = EMOJI_REGEX
            .get()
            .unwrap()
            .find_iter(&message.content)
            .count();
        let links = LINK_REGEX
            .get()
            .unwrap()
            .find_iter(&message.content)
            .count();

        SpamRecord {
            // Unfortunately, this clone is necessary, because `message` will be
            // dropped while we still need this.
            content: message.content.clone(),
            emoji: emoji as u8,
            links: links as u8,
            // `as` cast is safe for our purposes. If the message has more than
            // 255 attachments, `as` will give us a u8 with a value of 255.
            attachments: message.attachments.len() as u8,
            spoilers: spoilers as u8,
            sent_at: OffsetDateTime::parse(&message.timestamp, time::Format::Rfc3339).unwrap(),
        }
    }
}

type SpamHistory = HashMap<Snowflake, Arc<Mutex<VecDeque<SpamRecord>>>>;

fn exceeds_spam_thresholds(
    history: &VecDeque<SpamRecord>,
    current_record: &SpamRecord,
    config: &SpamFilter,
) -> FilterResult {
    let (emoji_sum, link_sum, attachment_sum, spoiler_sum, matching_duplicates) = history
        .iter()
        // Start with a value of 1 for matching_duplicates because the current spam record
        // is always a duplicate of itself.
        .fold(
            (
                current_record.emoji,
                current_record.links,
                current_record.attachments,
                current_record.spoilers,
                1u8,
            ),
            |(total_emoji, total_links, total_attachments, total_spoilers, total_duplicates),
             record| {
                (
                    total_emoji.saturating_add(record.emoji),
                    total_links.saturating_add(record.links),
                    total_attachments.saturating_add(record.attachments),
                    total_spoilers.saturating_add(record.spoilers),
                    total_duplicates
                        .saturating_add((record.content == current_record.content) as u8),
                )
            },
        );

    log::trace!(
        "Spam summary: {} emoji, {} links, {} attachments, {} spoilers, {} duplicates",
        emoji_sum,
        link_sum,
        attachment_sum,
        spoiler_sum,
        matching_duplicates
    );

    if config.emoji.is_some() && emoji_sum > config.emoji.unwrap() && current_record.emoji > 0 {
        Err("sent too many emoji".to_owned())
    } else if config.links.is_some() && link_sum > config.links.unwrap() && current_record.links > 0
    {
        Err("sent too many links".to_owned())
    } else if config.attachments.is_some()
        && attachment_sum > config.attachments.unwrap()
        && current_record.attachments > 0
    {
        Err("sent too many attachments".to_owned())
    } else if config.spoilers.is_some()
        && spoiler_sum > config.spoilers.unwrap()
        && current_record.spoilers > 0
    {
        Err("sent too many spoilers".to_owned())
    } else if config.duplicates.is_some() && matching_duplicates > config.duplicates.unwrap() {
        Err("sent too many duplicate messages".to_owned())
    } else {
        Ok(())
    }
}

fn init_globals() {
    // The Err case here is if the cell already has a value in it. In this case
    // we want to just ignore it. The only time this will happen is in tests,
    // where each test can call init_globals().
    let _ = ZALGO_REGEX
        .set(Regex::new("\\u0303|\\u035F|\\u034F|\\u0327|\\u031F|\\u0353|\\u032F|\\u0318|\\u0353|\\u0359|\\u0354").unwrap());
    let _ = INVITE_REGEX.set(Regex::new("discord.gg/(\\w+)").unwrap());
    let _ = LINK_REGEX.set(Regex::new("https?://([^/\\s]+)").unwrap());
    let _ = SPOILER_REGEX.set(Regex::new("||[^|]*||").unwrap());
    let _ = EMOJI_REGEX.set(
        Regex::new("\\p{Emoji_Presentation}|\\p{Emoji}\\uFE0F|\\p{Emoji_Modifier_Base}").unwrap(),
    );
    let _ = CUSTOM_EMOJI_REGEX.set(Regex::new("<a?:([^:]+):(\\d+)>").unwrap());
}

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

async fn check_spam_record(
    message: &Message,
    config: &SpamFilter,
    spam_history: Arc<RwLock<SpamHistory>>,
) -> FilterResult {
    let new_spam_record = SpamRecord::from_message(&message);
    let author_spam_history = {
        let read_history = spam_history.read().await;
        // This is tricky: We need to release the read lock, acquire a write lock, and
        // then insert the new history entry into the map.
        if !read_history.contains_key(&message.author.id) {
            drop(read_history);

            let new_history = Arc::new(Mutex::new(VecDeque::new()));
            let mut write_history = spam_history.write().await;
            write_history.insert(message.author.id, new_history.clone());
            new_history
        } else {
            read_history.get(&message.author.id).unwrap().clone()
        }
    };

    let mut spam_history = author_spam_history.lock().unwrap();

    let interval = Duration::from_secs(config.interval as u64);
    let now = OffsetDateTime::now_utc();
    let mut cleared_count = 0;
    loop {
        match spam_history.front() {
            Some(front) => {
                if now - front.sent_at > interval {
                    spam_history.pop_front();
                    cleared_count += 1;
                } else {
                    break;
                }
            }
            None => break,
        }
    }

    log::trace!(
        "Cleared {} spam records for user {}",
        cleared_count,
        message.author.id
    );

    let result = exceeds_spam_thresholds(&spam_history, &new_spam_record, &config);
    spam_history.push_back(new_spam_record);
    result
}

#[tokio::main]
async fn main() {
    dotenv::dotenv().ok();
    pretty_env_logger::init();

    init_globals();

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

    let intents = Intents::GUILD_MESSAGES;

    let client = std::sync::Arc::new(client);
    let cfg = std::sync::Arc::new(cfg);
    let spam_history = Arc::new(RwLock::new(SpamHistory::new()));

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
                                                check_spam_record(
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
                        log::debug!("INTERACTION: {:?}", interaction);
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
    use discordant::types::{Attachment, MessageStickerItem};

    use super::*;

    #[test]
    fn filter_words() {
        let rule = MessageFilterRule::Words {
            words: Regex::new("\\b(a|b)\\b").unwrap(),
        };

        assert_eq!(
            rule.check_match(&Message {
                content: "c".to_owned(),
                ..Default::default()
            }),
            Ok(())
        );

        assert_eq!(
            rule.check_match(&Message {
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
            rule.check_match(&Message {
                content: "c".to_owned(),
                ..Default::default()
            }),
            Ok(())
        );

        assert_eq!(
            rule.check_match(&Message {
                content: "a".to_owned(),
                ..Default::default()
            }),
            Err("matches regex a|b".to_owned())
        );
    }

    #[test]
    fn filter_zalgo() {
        init_globals();

        let rule = MessageFilterRule::Zalgo;

        assert_eq!(
            rule.check_match(&Message {
                content: "c".to_owned(),
                ..Default::default()
            }),
            Ok(())
        );

        assert_eq!(
            rule.check_match(&Message {
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

        assert_eq!(allow_rule.check_match(&png_message), Ok(()));

        assert_eq!(
            allow_rule.check_match(&gif_message),
            Err("contains unallowed content type image/gif".to_owned())
        );

        assert_eq!(
            deny_rule.check_match(&png_message),
            Err("contains denied content type image/png".to_owned())
        );

        assert_eq!(deny_rule.check_match(&gif_message), Ok(()));
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

        assert_eq!(allow_rule.check_match(&unknown_message), Ok(()));

        assert_eq!(
            deny_rule.check_match(&unknown_message),
            Err("unknown content type for attachment".to_owned())
        );
    }

    #[test]
    fn filter_invites() {
        init_globals();

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

        assert_eq!(allow_rule.check_match(&roblox_message), Ok(()));

        assert_eq!(
            allow_rule.check_match(&not_roblox_message),
            Err("contains unallowed invite asdf".to_owned())
        );

        assert_eq!(
            deny_rule.check_match(&roblox_message),
            Err("contains denied invite roblox".to_owned())
        );

        assert_eq!(deny_rule.check_match(&not_roblox_message), Ok(()));
    }

    #[test]
    fn filter_domains() {
        init_globals();

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

        assert_eq!(allow_rule.check_match(&roblox_message), Ok(()));

        assert_eq!(
            allow_rule.check_match(&not_roblox_message),
            Err("contains unallowed domain discord.com".to_owned())
        );

        assert_eq!(
            deny_rule.check_match(&roblox_message),
            Err("contains denied domain roblox.com".to_owned())
        );

        assert_eq!(deny_rule.check_match(&not_roblox_message), Ok(()));
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

        assert_eq!(allow_rule.check_match(&zero_sticker), Ok(()));

        assert_eq!(
            allow_rule.check_match(&non_zero_sticker),
            Err("contains unallowed sticker 1".to_owned())
        );

        assert_eq!(
            deny_rule.check_match(&zero_sticker),
            Err("contains denied sticker 0".to_owned())
        );

        assert_eq!(deny_rule.check_match(&non_zero_sticker), Ok(()));
    }
}
