use std::collections::HashMap;

use discordant::gateway::{connect_to_gateway, Event, Intents};
use discordant::http::{Client, CreateMessagePayload, DiscordHttpError};
use discordant::types::{Message, Snowflake};
use once_cell::sync::OnceCell;
use regex::Regex;
use serde::Deserialize;

static ZALGO_REGEX: OnceCell<Regex> = OnceCell::new();
static INVITE_REGEX: OnceCell<Regex> = OnceCell::new();
static LINK_REGEX: OnceCell<Regex> = OnceCell::new();

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
            if regex.is_err() {
                Err(serde::de::Error::custom(format!(
                    "unable to construct regex: {}",
                    regex.unwrap_err()
                )))
            } else {
                Ok(regex.unwrap())
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

type FilterResult = Result<(), String>;

impl Filter {
    fn check_match(&self, message: &Message) -> FilterResult {
        match self {
            Filter::Words { words } => {
                if let Some(captures) = words.captures(&message.content) {
                    Err(format!(
                        "contains word {}",
                        captures.get(1).unwrap().as_str()
                    ))
                } else {
                    Ok(())
                }
            }
            Filter::Regex { regexes } => {
                for regex in regexes {
                    if regex.is_match(&message.content) {
                        return Err(format!("matches regex {}", regex));
                    }
                }

                Ok(())
            }
            Filter::Zalgo => {
                let zalgo_regex = ZALGO_REGEX.get().unwrap();
                if zalgo_regex.is_match(&message.content) {
                    Err("contains zalgo".to_owned())
                } else {
                    Ok(())
                }
            }
            Filter::MimeType {
                mode,
                types,
                allow_unknown,
            } => {
                if message.attachments.iter().any(|a| a.content_type.is_none()) && !allow_unknown {
                    return Err("unknown content type for attachment".to_owned());
                }

                let mut attachment_types = message.attachments.iter().filter_map(|a| if let Some(content_type) = &a.content_type {
                    Some(content_type.as_str())
                } else {
                    None
                });

                filter_values(mode, "content type", &mut attachment_types, types)
            },
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
                    Ok(())
                }
            }
        }
    }
}

#[derive(Deserialize, Debug)]
struct SpamThreshold {
    count: u8,
    interval: u16,
}

#[derive(Deserialize, Debug)]
struct SpamConfig {
    emoji: Option<SpamThreshold>,
    duplicates: Option<SpamThreshold>,
    links: Option<SpamThreshold>,
    attachments: Option<SpamThreshold>,
}

#[derive(Deserialize, Debug)]
struct FilterConfig {
    rules: Vec<Filter>,
    spam: SpamConfig,
    exclude_channels: Vec<Snowflake>,
    include_channels: Vec<Snowflake>,
    exclude_roles: Vec<Snowflake>,
    actions: Vec<Action>,
}

#[derive(Deserialize, Debug)]
struct GuildConfig {
    filters: Vec<FilterConfig>,
}

#[derive(Deserialize, Debug)]
struct Config {
    guilds: HashMap<Snowflake, GuildConfig>,
}

#[tokio::main]
async fn main() {
    dotenv::dotenv().ok();
    pretty_env_logger::init();

    ZALGO_REGEX
        .set(Regex::new("[\\xCC\\xCD]").unwrap())
        .unwrap();
    INVITE_REGEX
        .set(Regex::new("discord.gg/(\\w+)").unwrap())
        .unwrap();
    LINK_REGEX
        .set(Regex::new("https?://([^/\\s]+)").unwrap())
        .unwrap();

    let discord_token =
        std::env::var("DISCORD_TOKEN").expect("Couldn't retrieve DISCORD_TOKEN variable");

    // Ugly: Strip out single-line comments from the source. serde_json doesn't
    // support comments, but config files kind of need them.
    let comment_regex = Regex::new("//[^\n]*\n").unwrap();
    let cfg_str =
        std::fs::read_to_string("chrysanthemum.cfg.json").expect("couldn't read config file");
    let cfg_json = comment_regex.replace_all(&cfg_str, "");
    let cfg: Config = serde_json::from_str(&cfg_json).expect("Couldn't deserialize config");
    println!("{:#?}", cfg);

    let client = discordant::http::Client::new(&discord_token);
    let gateway_info = client.get_gateway_info().await.unwrap();

    let intents = Intents::GUILD_MESSAGES;

    let client = std::sync::Arc::new(client);
    let cfg = std::sync::Arc::new(cfg);

    let mut gateway = connect_to_gateway(&gateway_info.url, discord_token, intents)
        .await
        .expect("Could not connect to gateway");
    loop {
        let event = gateway.next_event().await;

        if event.is_err() {
            log::error!("Error: {:?}", event.unwrap_err());
            break;
        } else {
            match event.unwrap() {
                Event::MessageCreate(message) => {
                    let cfg = cfg.clone();
                    let client = client.clone();
                    tokio::spawn(async move {
                        // guild_id will always be set in this case, because we
                        // will only ever receive guild messages via our intent.
                        if let Some(guild_config) = cfg.guilds.get(&message.guild_id.unwrap()) {
                            for filter in &guild_config.filters {
                                if filter
                                    .exclude_channels
                                    .iter()
                                    .any(|c| message.channel_id == *c)
                                {
                                    continue;
                                }

                                if !filter
                                    .include_channels
                                    .iter()
                                    .any(|c| message.channel_id == *c)
                                {
                                    continue;
                                }

                                if let Some(member_info) = &message.member {
                                    if filter
                                        .exclude_roles
                                        .iter()
                                        .any(|r| member_info.roles.contains(r))
                                    {
                                        continue;
                                    }
                                }

                                let filter_result = filter
                                    .rules
                                    .iter()
                                    .map(|f| f.check_match(&message))
                                    .find(|r| r.is_err())
                                    .unwrap_or(Ok(()));

                                if let Err(reason) = filter_result {
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
                _ => {}
            }
        }
    }
}
