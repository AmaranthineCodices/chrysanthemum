use std::collections::HashMap;

use serde::Deserialize;

use twilight_model::{channel::message::sticker::StickerId, id::{GuildId, RoleId, ChannelId, EmojiId}};

use regex::Regex;

fn deserialize_regex_pattern<'de, D>(de: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct RegexVisitor;
    impl<'de> serde::de::Visitor<'de> for RegexVisitor {
        type Value = String;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("word list")
        }

        fn visit_seq<V>(self, mut seq: V) -> Result<String, V::Error>
        where
            V: serde::de::SeqAccess<'de>,
        {
            let mut words = Vec::new();
            while let Some(word) = seq.next_element()? {
                words.push(regex::escape(word));
            }

            let pattern = words.join("|");
            Ok(pattern)
        }
    }

    de.deserialize_seq(RegexVisitor)
}

/// Deserializes a list of strings into a single regex that matches any of those
/// words, capturing the matching word. This allows for more performant matching
/// because the regex engine is better at doing this kind of test than we are.
fn deserialize_word_regex<'de, D>(de: D) -> Result<Regex, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let pattern = deserialize_regex_pattern(de);
    
    match pattern {
        Ok(mut pattern) => {
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
        },
        Err(e) => Err(e)
    }
}

fn deserialize_substring_regex<'de, D>(de: D) -> Result<Regex, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let pattern = deserialize_regex_pattern(de);
    
    match pattern {
        Ok(pattern) => {
            let regex = Regex::new(&pattern);

            match regex {
                Ok(regex) => Ok(regex),
                Err(err) => Err(serde::de::Error::custom(format!(
                    "unable to construct regex: {}",
                    err
                ))),
            }
        },
        Err(e) => Err(e)
    }
}

#[derive(Deserialize, Debug)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum MessageFilterAction {
    /// Delete the offending piece of content.
    Delete,
    /// Send a message to a channel.
    SendMessage {
        channel_id: ChannelId,
        content: String,
        batch: Option<bool>,
    },
}

#[derive(Deserialize, Debug)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ReactionFilterAction {
    Delete,
    SendMessage {
        channel_id: ChannelId,
        content: String,
        batch: Option<bool>,
    },
}

#[derive(Deserialize, Debug)]
pub enum FilterMode {
    #[serde(rename = "allow")]
    AllowList,
    #[serde(rename = "deny")]
    DenyList,
}

#[derive(Deserialize, Debug, Default)]
pub struct Scoping {
    /// Which channels to exclude.
    pub exclude_channels: Option<Vec<ChannelId>>,
    /// Which channels to include.
    pub include_channels: Option<Vec<ChannelId>>,
    /// Which roles to exclude.
    pub exclude_roles: Option<Vec<RoleId>>,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageFilterRule {
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
        /// Sometimes an attachment won't have a MIME type attached. If this is
        /// the case, what do we do? This field controls this behavior - we can
        /// either ignore it, or reject it out of an abundance of caution.
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
    StickerId {
        mode: FilterMode,
        stickers: Vec<StickerId>,
    },
    StickerName {
        // Note: In the config format, this is an array of strings, not one
        // regex pattern.
        #[serde(deserialize_with = "deserialize_substring_regex")]
        stickers: Regex,
    },
    EmojiName {
        // Note: In the config format, this is an array of strings, not one
        // regex pattern.
        #[serde(deserialize_with = "deserialize_substring_regex")]
        names: Regex,
    },
}

#[derive(Deserialize, Debug, Default)]
pub struct SpamFilter {
    /// How many emoji in a given interval constitute spam.
    pub emoji: Option<u8>,
    /// How many duplicates in a given interval constitute spam.
    pub duplicates: Option<u8>,
    /// How many links in a given interval constitute spam.
    pub links: Option<u8>,
    /// How many attachments in a given interval constitute spam.
    pub attachments: Option<u8>,
    /// How many spoilers in a given interval constitute spam.
    pub spoilers: Option<u8>,
    /// How many mentions in a given interval constitute spam.
    pub mentions: Option<u8>,
    /// How long, in seconds, to consider messages for spam.
    pub interval: u16,
    /// What actions to take when a message is considered spam.
    pub actions: Option<Vec<MessageFilterAction>>,
    /// Scoping rules to apply to the spam filter.
    pub scoping: Option<Scoping>,
}

#[derive(Deserialize, Debug, Default)]
pub struct MessageFilter {
    /// Which rules to match messages against.
    pub rules: Vec<MessageFilterRule>,
    /// What scoping to use for this rule.
    pub scoping: Option<Scoping>,
    /// What actions to take when a message matches a filter.
    pub actions: Option<Vec<MessageFilterAction>>,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReactionFilterRule {
    /// Filter default emoji.
    Default {
        mode: FilterMode,
        emoji: Vec<String>
    },
    /// Filter custom emoji by ID.
    CustomId {
        mode: FilterMode,
        emoji: Vec<EmojiId>,
    },
    /// Filter custom emoji by name.
    CustomName {
        // Note: In the config format, this is an array of strings, not one
        // regex pattern.
        #[serde(deserialize_with = "deserialize_substring_regex")]
        names: Regex,
    }
}

#[derive(Deserialize, Debug)]
pub struct ReactionFilter {
    pub rules: Vec<ReactionFilterRule>,
    pub scoping: Option<Scoping>,
    pub actions: Option<Vec<MessageFilterAction>>,
}

#[derive(Deserialize, Debug)]
pub struct SlashCommands {
    /// Which roles are allowed to use slash commands.
    pub roles: Vec<RoleId>,
}

#[derive(Deserialize, Debug)]
pub struct Notifications {
    /// Which channel to send notifications to.
    pub channel: ChannelId,
    /// Which roles to ping for notifications.
    pub ping_roles: Option<Vec<RoleId>>,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
pub enum UsernameFilterRule {
    Substring {
        // Note: In the config format, this is an array of strings, not one
        // regex pattern.
        #[serde(deserialize_with = "deserialize_substring_regex")]
        substrings: Regex,
    },
    Regex {
        #[serde(with = "serde_regex")]
        regexes: Vec<Regex>,
    }
}

#[derive(Deserialize, Debug)]
pub enum UsernameFilterAction {
    SendMessage {
        channel_id: ChannelId,
        content: String,
    },
}

#[derive(Deserialize, Debug)]
pub struct UsernameFilter {
    /// Rules to apply to usernames.
    pub rules: Vec<UsernameFilterRule>,
    /// Actions to take when a username matches one of the rules.
    pub actions: Vec<UsernameFilterAction>,
}

#[derive(Deserialize, Debug)]
pub struct GuildConfig {
    pub notifications: Option<Notifications>,
    pub slash_commands: Option<SlashCommands>,
    pub default_scoping: Option<Scoping>,
    pub default_actions: Option<Vec<MessageFilterAction>>,
    pub messages: Option<Vec<MessageFilter>>,
    pub reactions: Option<Vec<ReactionFilter>>,
    pub spam: Option<SpamFilter>,
    pub usernames: Option<UsernameFilter>,
    /// Whether to include bots. This is used for integration tests, where two
    /// bots interact with each other. This should not be set in most production
    /// environments. Chrysanthemum will always ignore itself.
    #[serde(default)]
    pub include_bots: bool,
}

#[derive(Deserialize, Debug)]
pub struct InfluxConfig {
    pub url: String,
    pub database: String,
    pub token: String,
}

#[derive(Deserialize, Debug)]
pub struct SentryConfig {
    pub url: String,
}

#[derive(Deserialize, Debug)]
pub struct Config {
    pub guilds: HashMap<GuildId, GuildConfig>,
    pub influx: Option<InfluxConfig>,
    pub sentry: Option<SentryConfig>,
}

fn validate_scoping(scoping: &Scoping, context: &str, errors: &mut Vec<String>) {
    if scoping.exclude_channels.is_some() && scoping.include_channels.is_some() {
        errors.push(format!("in {}, scoping rule specifies both exclude_channels and include_channels. Specify only one.", context));
    }

    if scoping.exclude_channels.is_some() && scoping.exclude_channels.as_ref().unwrap().len() == 0 {
        errors.push(format!("in {}, scoping rule specifies an empty exclude_channels; omit the key instead.", context));
    }

    if scoping.include_channels.is_some() && scoping.include_channels.as_ref().unwrap().len() == 0 {
        errors.push(format!("in {}, scoping rule specifies an empty include_channels; omit the key instead.", context));
    }

    if scoping.exclude_roles.is_some() && scoping.exclude_roles.as_ref().unwrap().len() == 0 {
        errors.push(format!("in {}, scoping rule specifies an empty exclude_roles; omit the key instead.", context));
    }
}

pub fn validate_config(config: &Config) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();

    for (guild_id, guild) in &config.guilds {
        if let Some(slash_commands) = &guild.slash_commands {
            if slash_commands.roles.len() == 0 {
                errors.push(format!("in guild {}, slash_commands.roles is empty - no roles will be able to use slash commands.", guild_id));
            }
        }

        if let Some(scoping) = &guild.default_scoping {
            validate_scoping(scoping, &format!("guild {} default scoping", guild_id), &mut errors);
        }

        let mut has_default_actions = false;
        if let Some(actions) = &guild.default_actions {
            if actions.len() == 0 {
                errors.push(format!("in guild {}, default_actions is specified but is empty.", guild_id));
            } else {
                has_default_actions = true;
            }
        }

        if let Some(notifications) = &guild.notifications {
            if let Some(roles) = &notifications.ping_roles {
                if roles.len() == 0 {
                    errors.push(format!("in guild {} notification settings, ping_roles is specified but is empty; omit the key.", guild_id));
                }
            }
        }

        if let Some(spam) = &guild.spam {
            if let Some(scoping) = spam.scoping.as_ref() {
                validate_scoping(scoping, &format!("guild {} spam scoping", guild_id), &mut errors);
            }
            
            if let Some(actions) = &spam.actions {
                if actions.len() == 0 {
                    errors.push(format!("in guild {} spam config, actions is specified but is empty.", guild_id));
                }
            } else if !has_default_actions {
                errors.push(format!("in guild {} spam config, no actions are specified and there are no default actions for this guild.", guild_id));
            }

            if spam.emoji.is_none() && spam.attachments.is_none() && spam.duplicates.is_none() && spam.links.is_none() && spam.spoilers.is_none() {
                errors.push(format!("in guild {} spam config, no spam thresholds are specified. Spam filtering will have no effects.", guild_id));
            }
        }

        if let Some(usernames) = &guild.usernames {
            if usernames.actions.len() == 0 {
                errors.push(format!("in guild {} username config, actions is empty.", guild_id));
            }

            if usernames.rules.len() == 0 {
                errors.push(format!("in guild {} username config, rules is empty.", guild_id));
            }
        }

        if let Some(messages) = &guild.messages {
            if messages.len() == 0 {
                errors.push(format!("in guild {}, messages is empty; omit the key.", guild_id));
            }

            for (i, filter) in messages.iter().enumerate() {
                match &filter.actions {
                    Some(actions) => {
                        if actions.len() == 0 {
                            errors.push(format!("in guild {}, message filter {} has an empty actions array; omit the key to use default actions", guild_id, i));
                        }
                    },
                    None => {
                        if !has_default_actions {
                            errors.push(format!("in guild {}, message filter {} does not specify actions, but this guild has no default actions.", guild_id, i));
                        }
                    }
                }

                if let Some(scoping) = &filter.scoping {
                    validate_scoping(scoping, &format!("guild {}, message filter {}", guild_id, i), &mut errors);
                }

                if filter.rules.len() == 0 {
                    errors.push(format!("in guild {}, message filter {} has no rules", guild_id, i));
                }
            }
        }

        if let Some(reactions) = &guild.reactions {
            if reactions.len() == 0 {
                errors.push(format!("in guild {}, reactions is specified but is empty; omit the key to disable reaction filtering", guild_id));
            }

            for (i, filter) in reactions.iter().enumerate() {
                match &filter.actions {
                    Some(actions) => {
                        if actions.len() == 0 {
                            errors.push(format!("in guild {}, reaction filter {} has an empty actions array; omit the key to use default actions", guild_id, i));
                        }
                    },
                    None => {
                        if !has_default_actions {
                            errors.push(format!("in guild {}, reaction filter {} does not specify actions, but this guild has no default actions.", guild_id, i));
                        }
                    }
                }

                if let Some(scoping) = &filter.scoping {
                    validate_scoping(scoping, &format!("guild {}, reaction filter {}", guild_id, i), &mut errors);
                }

                if filter.rules.len() == 0 {
                    errors.push(format!("in guild {}, reaction filter {} has no rules", guild_id, i));
                }
            }
        }
    }

    if errors.len() > 0 {
        Err(errors)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn deserialize_word_regex() {
        let json = r#"
        {
            "type": "words",
            "words": ["a", "b", "a(b)"]
        }
        "#;

        let rule: MessageFilterRule =
            serde_json::from_str(&json).expect("couldn't deserialize MessageFilterRule");

        if let MessageFilterRule::Words { words } = rule {
            assert_eq!(words.to_string(), "\\b(a|b|a\\(b\\))\\b");
        } else {
            assert!(false, "deserialized wrong filter");
        }
    }
}
