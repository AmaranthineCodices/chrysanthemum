use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use discordant::gateway::{connect_to_gateway, Event, Intents};
use discordant::http::{Client, CreateMessagePayload, DiscordHttpError};
use discordant::types::{Message, Snowflake};
use once_cell::sync::OnceCell;
use regex::Regex;
use serde::Deserialize;
use time::OffsetDateTime;
use tokio::sync::RwLock;

static ZALGO_REGEX: OnceCell<Regex> = OnceCell::new();
static INVITE_REGEX: OnceCell<Regex> = OnceCell::new();
static LINK_REGEX: OnceCell<Regex> = OnceCell::new();
static SPOILER_REGEX: OnceCell<Regex> = OnceCell::new();
static EMOJI_REGEX: OnceCell<Regex> = OnceCell::new();

#[derive(Deserialize, Debug)]
#[serde(tag = "action", rename_all = "snake_case")]
enum Action {
    Delete,
    SendMessage {
        channel_id: Snowflake,
        content: String,
    },
}

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

/// Deserializes a list of strings into a single regex that matches any of those
/// words, capturing the matching word. This allows for more performant matching
/// because the regex engine is better at doing this kind of test than we are.
fn deserialize_word_regex<'de, D>(de: D) -> Result<Regex, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct WordRegexVisitor;
    impl<'de> serde::de::Visitor<'de> for WordRegexVisitor {
        type Value = Regex;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("word list")
        }

        fn visit_seq<V>(self, mut seq: V) -> Result<Regex, V::Error>
        where
            V: serde::de::SeqAccess<'de>,
        {
            let mut words = Vec::new();
            while let Some(word) = seq.next_element()? {
                words.push(regex::escape(word));
            }

            let mut pattern = words.join("|");
            pattern.insert_str(0, "\\b(");
            pattern.push_str(")\\b");

            let regex = Regex::new(&pattern);

            match regex {
                Ok(regex) => Ok(regex),
                Err(err) => Err(serde::de::Error::custom(format!(
                    "unable to construct regex: {}",
                    err
                ))),
            }
        }
    }

    de.deserialize_seq(WordRegexVisitor)
}

#[derive(Deserialize, Debug)]
enum FilterMode {
    #[serde(rename = "allow")]
    AllowList,
    #[serde(rename = "deny")]
    DenyList,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Filter {
    Words {
        // Note: In the config format, this is an array of strings, not one
        // regex pattern.
        #[serde(deserialize_with = "deserialize_word_regex")]
        words: Regex,
    },
    Regex {
        #[serde(with = "serde_regex")]
        regexes: Vec<Regex>,
    },
    Zalgo,
    MimeType {
        mode: FilterMode,
        types: Vec<String>,
        // Sometimes an attachment won't have a MIME type attached. If this is
        // the case, what do we do? This field controls this behavior - we can
        // either ignore it, or reject it out of an abundance of caution.
        allow_unknown: bool,
    },
    Invite {
        mode: FilterMode,
        invites: Vec<String>,
    },
    Link {
        mode: FilterMode,
        domains: Vec<String>,
    },
    Sticker {
        mode: FilterMode,
        stickers: Vec<Snowflake>,
    },
}

#[derive(Debug, PartialEq, Eq)]
enum FilterResult {
    Ok,
    Pass,
    Failed(String),
}

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
            .map(|v| FilterResult::Failed(format!("contains unallowed {} {}", context, v))),
        FilterMode::DenyList => values
            .find(|v| filter_values.iter().any(|f| f == v))
            .map(|v| FilterResult::Failed(format!("contains denied {} {}", context, v))),
    };

    result.unwrap_or(FilterResult::Ok)
}

impl Filter {
    fn check_match(&self, message: &Message) -> FilterResult {
        match self {
            Filter::Words { words } => {
                if let Some(captures) = words.captures(&message.content) {
                    FilterResult::Failed(format!(
                        "contains word {}",
                        captures.get(1).unwrap().as_str()
                    ))
                } else {
                    FilterResult::Ok
                }
            }
            Filter::Regex { regexes } => {
                for regex in regexes {
                    if regex.is_match(&message.content) {
                        return FilterResult::Failed(format!("matches regex {}", regex));
                    }
                }

                FilterResult::Ok
            }
            Filter::Zalgo => {
                let zalgo_regex = ZALGO_REGEX.get().unwrap();
                if zalgo_regex.is_match(&message.content) {
                    FilterResult::Failed("contains zalgo".to_owned())
                } else {
                    FilterResult::Ok
                }
            }
            Filter::MimeType {
                mode,
                types,
                allow_unknown,
            } => {
                if message.attachments.iter().any(|a| a.content_type.is_none()) && !allow_unknown {
                    return FilterResult::Failed("unknown content type for attachment".to_owned());
                }

                let mut attachment_types = message
                    .attachments
                    .iter()
                    .filter_map(|a| a.content_type.as_deref());
                filter_values(mode, "content type", &mut attachment_types, types)
            }
            Filter::Invite { mode, invites } => {
                let invite_regex = INVITE_REGEX.get().unwrap();
                let mut invite_ids = invite_regex
                    .captures_iter(&message.content)
                    .map(|c| c.get(1).unwrap().as_str());
                filter_values(mode, "invite", &mut invite_ids, invites)
            }
            Filter::Link { mode, domains } => {
                let link_regex = LINK_REGEX.get().unwrap();
                let mut link_domains = link_regex
                    .captures_iter(&message.content)
                    .map(|c| c.get(1).unwrap().as_str());
                filter_values(mode, "domain", &mut link_domains, domains)
            }
            Filter::Sticker { mode, stickers } => {
                if let Some(message_stickers) = &message.sticker_items {
                    filter_values(
                        mode,
                        "sticker",
                        &mut message_stickers.iter().map(|s| s.id),
                        stickers,
                    )
                } else {
                    FilterResult::Ok
                }
            }
        }
    }
}

#[derive(Deserialize, Debug, Default)]
struct SpamConfig {
    emoji: Option<u8>,
    duplicates: Option<u8>,
    links: Option<u8>,
    attachments: Option<u8>,
    spoilers: Option<u8>,
    interval: u16,
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
        let spoilers = SPOILER_REGEX.get().unwrap().find_iter(&message.content).count();
        let emoji = EMOJI_REGEX.get().unwrap().find_iter(&message.content).count();
        let links = LINK_REGEX.get().unwrap().find_iter(&message.content).count();

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
    config: &SpamConfig,
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
                    total_duplicates.saturating_add((record.content == current_record.content) as u8),
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
        FilterResult::Failed("sent too many emoji".to_owned())
    } else if config.links.is_some() && link_sum > config.links.unwrap() && current_record.links > 0
    {
        FilterResult::Failed("sent too many links".to_owned())
    } else if config.attachments.is_some()
        && attachment_sum > config.attachments.unwrap()
        && current_record.attachments > 0
    {
        FilterResult::Failed("sent too many attachments".to_owned())
    } else if config.spoilers.is_some()
        && spoiler_sum > config.spoilers.unwrap()
        && current_record.spoilers > 0
    {
        FilterResult::Failed("sent too many spoilers".to_owned())
    } else if config.duplicates.is_some() && matching_duplicates > config.duplicates.unwrap() {
        FilterResult::Failed("sent too many duplicate messages".to_owned())
    } else {
        FilterResult::Ok
    }
}

#[derive(Deserialize, Debug, Default)]
struct FilterConfig {
    rules: Vec<Filter>,
    spam: SpamConfig,
    exclude_channels: Vec<Snowflake>,
    include_channels: Vec<Snowflake>,
    exclude_roles: Vec<Snowflake>,
    actions: Vec<Action>,
}

impl FilterConfig {
    fn filter_message(&self, message: &Message) -> FilterResult {
        if self.include_channels.is_empty()
            && self
                .exclude_channels
                .iter()
                .any(|c| message.channel_id == *c)
        {
            return FilterResult::Pass;
        }

        if !self
            .include_channels
            .iter()
            .any(|c| message.channel_id == *c)
        {
            return FilterResult::Pass;
        }

        if let Some(member_info) = &message.member {
            if self
                .exclude_roles
                .iter()
                .any(|r| member_info.roles.contains(r))
            {
                return FilterResult::Pass;
            }
        }

        self.rules
            .iter()
            .map(|f| f.check_match(&message))
            .find(|r| matches!(r, FilterResult::Failed(_)))
            .unwrap_or(FilterResult::Ok)
    }
}

#[derive(Deserialize, Debug)]
struct GuildConfig {
    filters: Vec<FilterConfig>,
}

#[derive(Deserialize, Debug)]
struct Config {
    guilds: HashMap<Snowflake, GuildConfig>,
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
    let _ = EMOJI_REGEX.set(Regex::new("\\p{Emoji_Presentation}|\\p{Emoji}\\uFE0F|\\p{Emoji_Modifier_Base}|<a?:[^:]+:\\d+>").unwrap());
}

#[derive(Debug)]
struct BotState {
    spam: SpamHistory,
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

    let client = discordant::http::Client::new(&discord_token);
    let gateway_info = client.get_gateway_info().await.unwrap();

    let intents = Intents::GUILD_MESSAGES;

    let client = std::sync::Arc::new(client);
    let cfg = std::sync::Arc::new(cfg);
    let spam_history = Arc::new(RwLock::new(SpamHistory::new()));

    let mut gateway = connect_to_gateway(&gateway_info.url, discord_token, intents)
        .await
        .expect("Could not connect to gateway");
    loop {
        let event = gateway.next_event().await;

        match event {
            Ok(event) => {
                if let Event::MessageCreate(message) = event {
                    let cfg = cfg.clone();
                    let client = client.clone();
                    let spam_history = spam_history.clone();
                    tokio::spawn(async move {
                        // guild_id will always be set in this case, because we
                        // will only ever receive guild messages via our intent.
                        if let Some(guild_config) = cfg.guilds.get(&message.guild_id.unwrap()) {
                            for filter in &guild_config.filters {
                                let filter_result = filter.filter_message(&message);

                                if matches!(filter_result, FilterResult::Pass) {
                                    continue
                                }

                                let new_spam_record = SpamRecord::from_message(&message);
                                let author_spam_history = {
                                    let read_history = spam_history.read().await;
                                    // This is tricky: We need to release the read lock, acquire a write lock, and
                                    // then insert the new history entry into the map.
                                    if !read_history.contains_key(&message.author.id) {
                                        drop(read_history);

                                        let new_history = Arc::new(Mutex::new(VecDeque::new()));
                                        let mut write_history = spam_history.write().await;
                                        write_history
                                            .insert(message.author.id, new_history.clone());
                                        drop(write_history);
                                        new_history
                                    } else {
                                        read_history.get(&message.author.id).unwrap().clone()
                                    }
                                };

                                let spam_result = if matches!(filter_result, FilterResult::Failed(_)) {
                                    FilterResult::Ok
                                } else {
                                    let mut spam_history = author_spam_history.lock().unwrap();

                                    let interval = Duration::from_secs(filter.spam.interval as u64);
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

                                    let result = exceeds_spam_thresholds(
                                        &spam_history,
                                        &new_spam_record,
                                        &filter.spam,
                                    );
                                    spam_history.push_back(new_spam_record);
                                    result
                                };

                                let result = if matches!(filter_result, FilterResult::Ok) {
                                    spam_result
                                } else {
                                    filter_result
                                };

                                if let FilterResult::Failed(reason) = result {
                                    for action in &filter.actions {
                                        action
                                            .do_action(&reason, &message, &client)
                                            .await
                                            .expect("Couldn't perform action");
                                    }

                                    break;
                                }
                            }
                        }
                    });
                }
            }
            Err(err) => {
                log::error!("Error: {:?}", err);
                break;
            }
        }
    }
}

#[cfg(test)]
mod test {
    use discordant::types::{Attachment, MessageGuildMemberInfo, MessageStickerItem};

    use super::*;

    #[test]
    fn filter_words() {
        let rule = Filter::Words {
            words: Regex::new("\\b(a|b)\\b").unwrap(),
        };

        assert_eq!(
            rule.check_match(&Message {
                content: "c".to_owned(),
                ..Default::default()
            }),
            FilterResult::Ok
        );

        assert_eq!(
            rule.check_match(&Message {
                content: "a".to_owned(),
                ..Default::default()
            }),
            FilterResult::Failed("contains word a".to_owned())
        );
    }

    #[test]
    fn filter_regex() {
        let rule = Filter::Regex {
            regexes: vec![Regex::new("a|b").unwrap()],
        };

        assert_eq!(
            rule.check_match(&Message {
                content: "c".to_owned(),
                ..Default::default()
            }),
            FilterResult::Ok
        );

        assert_eq!(
            rule.check_match(&Message {
                content: "a".to_owned(),
                ..Default::default()
            }),
            FilterResult::Failed("matches regex a|b".to_owned())
        );
    }

    #[test]
    fn filter_zalgo() {
        init_globals();

        let rule = Filter::Zalgo;

        assert_eq!(
            rule.check_match(&Message {
                content: "c".to_owned(),
                ..Default::default()
            }),
            FilterResult::Ok
        );

        assert_eq!(
            rule.check_match(&Message {
                content: "t̸͈͈̒̑͛ê̷͓̜͎s̴̡͍̳͊t̴̪͙́̚".to_owned(),
                ..Default::default()
            }),
            FilterResult::Failed("contains zalgo".to_owned())
        );
    }

    #[test]
    fn filter_mime_type() {
        let allow_rule = Filter::MimeType {
            mode: FilterMode::AllowList,
            types: vec!["image/png".to_owned()],
            allow_unknown: true,
        };

        let deny_rule = Filter::MimeType {
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

        assert_eq!(allow_rule.check_match(&png_message), FilterResult::Ok);

        assert_eq!(
            allow_rule.check_match(&gif_message),
            FilterResult::Failed("contains unallowed content type image/gif".to_owned())
        );

        assert_eq!(
            deny_rule.check_match(&png_message),
            FilterResult::Failed("contains denied content type image/png".to_owned())
        );

        assert_eq!(deny_rule.check_match(&gif_message), FilterResult::Ok);
    }

    #[test]
    fn filter_unknown_mime_type() {
        let allow_rule = Filter::MimeType {
            mode: FilterMode::AllowList,
            types: vec!["image/png".to_owned()],
            allow_unknown: true,
        };

        let deny_rule = Filter::MimeType {
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

        assert_eq!(allow_rule.check_match(&unknown_message), FilterResult::Ok);

        assert_eq!(
            deny_rule.check_match(&unknown_message),
            FilterResult::Failed("unknown content type for attachment".to_owned())
        );
    }

    #[test]
    fn filter_invites() {
        init_globals();

        let allow_rule = Filter::Invite {
            mode: FilterMode::AllowList,
            invites: vec!["roblox".to_owned()],
        };

        let deny_rule = Filter::Invite {
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

        assert_eq!(allow_rule.check_match(&roblox_message), FilterResult::Ok);

        assert_eq!(
            allow_rule.check_match(&not_roblox_message),
            FilterResult::Failed("contains unallowed invite asdf".to_owned())
        );

        assert_eq!(
            deny_rule.check_match(&roblox_message),
            FilterResult::Failed("contains denied invite roblox".to_owned())
        );

        assert_eq!(deny_rule.check_match(&not_roblox_message), FilterResult::Ok);
    }

    #[test]
    fn filter_domains() {
        init_globals();

        let allow_rule = Filter::Link {
            mode: FilterMode::AllowList,
            domains: vec!["roblox.com".to_owned()],
        };

        let deny_rule = Filter::Link {
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

        assert_eq!(allow_rule.check_match(&roblox_message), FilterResult::Ok);

        assert_eq!(
            allow_rule.check_match(&not_roblox_message),
            FilterResult::Failed("contains unallowed domain discord.com".to_owned())
        );

        assert_eq!(
            deny_rule.check_match(&roblox_message),
            FilterResult::Failed("contains denied domain roblox.com".to_owned())
        );

        assert_eq!(deny_rule.check_match(&not_roblox_message), FilterResult::Ok);
    }

    #[test]
    fn filter_stickers() {
        let allow_rule = Filter::Sticker {
            mode: FilterMode::AllowList,
            stickers: vec![Snowflake::new(0)],
        };

        let deny_rule = Filter::Sticker {
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

        assert_eq!(allow_rule.check_match(&zero_sticker), FilterResult::Ok);

        assert_eq!(
            allow_rule.check_match(&non_zero_sticker),
            FilterResult::Failed("contains unallowed sticker 1".to_owned())
        );

        assert_eq!(
            deny_rule.check_match(&zero_sticker),
            FilterResult::Failed("contains denied sticker 0".to_owned())
        );

        assert_eq!(deny_rule.check_match(&non_zero_sticker), FilterResult::Ok);
    }

    #[test]
    fn deserialize_word_regex() {
        let json = r#"
        {
            "type": "words",
            "words": ["a", "b", "a(b)"]
        }
        "#;

        let rule: Filter = serde_json::from_str(&json).expect("couldn't deserialize Filter");

        if let Filter::Words { words } = rule {
            assert_eq!(words.to_string(), "\\b(a|b|a\\(b\\))\\b");
        } else {
            assert!(false, "deserialized wrong filter");
        }
    }

    #[test]
    fn exclude_channels() {
        let cfg = FilterConfig {
            rules: vec![Filter::Words {
                words: Regex::new("\\b(a)\\b").unwrap(),
            }],
            exclude_channels: vec![Snowflake::new(1)],
            ..Default::default()
        };

        let message = Message {
            content: "a".to_owned(),
            channel_id: Snowflake::new(1),
            ..Default::default()
        };

        let result = cfg.filter_message(&message);
        assert_eq!(result, FilterResult::Pass);
    }

    #[test]
    fn include_channels() {
        let cfg = FilterConfig {
            rules: vec![Filter::Words {
                words: Regex::new("\\b(a)\\b").unwrap(),
            }],
            include_channels: vec![Snowflake::new(1)],
            ..Default::default()
        };

        let message_0 = Message {
            content: "a".to_owned(),
            channel_id: Snowflake::new(0),
            ..Default::default()
        };

        let message_1 = Message {
            content: "a".to_owned(),
            channel_id: Snowflake::new(1),
            ..Default::default()
        };

        let result_0 = cfg.filter_message(&message_0);
        assert_eq!(result_0, FilterResult::Pass);

        let result_1 = cfg.filter_message(&message_1);
        assert_eq!(result_1, FilterResult::Failed("contains word a".to_owned()));
    }

    #[test]
    fn exclude_channels_only_if_no_include_channels() {
        let cfg = FilterConfig {
            rules: vec![Filter::Words {
                words: Regex::new("\\b(a)\\b").unwrap(),
            }],
            exclude_channels: vec![Snowflake::new(1)],
            include_channels: vec![Snowflake::new(1)],
            ..Default::default()
        };

        let message = Message {
            content: "a".to_owned(),
            channel_id: Snowflake::new(1),
            ..Default::default()
        };

        let result = cfg.filter_message(&message);
        assert_eq!(result, FilterResult::Failed("contains word a".to_owned()));
    }

    #[test]
    fn exclude_roles() {
        let cfg = FilterConfig {
            rules: vec![Filter::Words {
                words: Regex::new("\\b(a)\\b").unwrap(),
            }],
            exclude_roles: vec![Snowflake::new(0)],
            ..Default::default()
        };

        let message = Message {
            content: "a".to_owned(),
            member: Some(MessageGuildMemberInfo {
                roles: vec![Snowflake::new(0)],
                ..Default::default()
            }),
            ..Default::default()
        };

        let result = cfg.filter_message(&message);
        assert_eq!(result, FilterResult::Pass);
    }
}
