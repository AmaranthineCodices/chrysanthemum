use std::{
    borrow::Cow,
    collections::HashMap,
    path::{Path, PathBuf},
};

use eyre::{Context, Result};
use serde::Deserialize;

use twilight_model::id::{
    marker::{ChannelMarker, EmojiMarker, GuildMarker, RoleMarker, StickerMarker},
    Id,
};

use regex::{Regex, RegexBuilder, RegexSet};

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
            while let Some(word) = seq.next_element::<Cow<'de, str>>()? {
                words.push(regex::escape(&word));
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
            let regex = RegexBuilder::new(&pattern).case_insensitive(true).build();

            match regex {
                Ok(regex) => Ok(regex),
                Err(err) => Err(serde::de::Error::custom(format!(
                    "unable to construct regex: {}",
                    err
                ))),
            }
        }
        Err(e) => Err(e),
    }
}

fn deserialize_substring_regex<'de, D>(de: D) -> Result<Regex, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let pattern = deserialize_regex_pattern(de);

    match pattern {
        Ok(pattern) => {
            let regex = RegexBuilder::new(&pattern).case_insensitive(true).build();

            match regex {
                Ok(regex) => Ok(regex),
                Err(err) => Err(serde::de::Error::custom(format!(
                    "unable to construct regex: {}",
                    err
                ))),
            }
        }
        Err(e) => Err(e),
    }
}

#[derive(Deserialize, Debug)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum MessageFilterAction {
    /// Delete the offending piece of content.
    Delete,
    /// Send a message to a channel.
    SendMessage {
        channel_id: Id<ChannelMarker>,
        content: String,
        requires_armed: bool,
    },
    /// Ban the user who sent the offending piece of content.
    Ban {
        // Reason used in the ban's audit log.
        reason: String,
        // The period over which to remove the banned user's messages, in seconds.
        delete_message_seconds: u32,
    },
    /// Kick the user who sent the offending piece of content.
    Kick {
        reason: String,
    },
    /// Timeout the user who sent the offending piece of content.
    Timeout {
        reason: String,
        /// How long to mute the user for, in seconds.
        duration: i64,
    },
    SendLog {
        channel_id: Id<ChannelMarker>,
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
    pub exclude_channels: Option<Vec<Id<ChannelMarker>>>,
    /// Which channels to include.
    pub include_channels: Option<Vec<Id<ChannelMarker>>>,
    /// Which roles to exclude.
    pub exclude_roles: Option<Vec<Id<RoleMarker>>>,
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
    Substring {
        #[serde(deserialize_with = "deserialize_substring_regex")]
        substrings: Regex,
    },
    Regex {
        #[serde(with = "serde_regex")]
        regexes: RegexSet,
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
        stickers: Vec<Id<StickerMarker>>,
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
    pub name: String,
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
        emoji: Vec<String>,
    },
    /// Filter custom emoji by ID.
    CustomId {
        mode: FilterMode,
        emoji: Vec<Id<EmojiMarker>>,
    },
    /// Filter custom emoji by name.
    CustomName {
        // Note: In the config format, this is an array of strings, not one
        // regex pattern.
        #[serde(deserialize_with = "deserialize_substring_regex")]
        names: Regex,
    },
}

#[derive(Deserialize, Debug)]
pub struct ReactionFilter {
    pub name: String,
    pub rules: Vec<ReactionFilterRule>,
    pub scoping: Option<Scoping>,
    pub actions: Option<Vec<MessageFilterAction>>,
}

#[derive(Deserialize, Debug)]
pub struct SlashCommands {
    pub enabled: bool,
}

#[derive(Deserialize, Debug)]
pub struct Notifications {
    /// Which channel to send notifications to.
    pub channel: Id<ChannelMarker>,
    /// Which roles to ping for notifications.
    pub ping_roles: Option<Vec<Id<RoleMarker>>>,
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
    },
}

#[derive(Deserialize, Debug)]
pub enum UsernameFilterAction {
    SendMessage {
        channel_id: Id<ChannelMarker>,
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
    pub report_every_n: usize,
}

#[derive(Deserialize, Debug)]
pub struct SentryConfig {
    pub url: String,
    pub sample_rate: Option<f32>,
}

#[derive(Deserialize, Debug)]
pub struct Config {
    pub guild_config_dir: PathBuf,
    pub active_guilds: Vec<Id<GuildMarker>>,
    pub influx: Option<InfluxConfig>,
    pub sentry: Option<SentryConfig>,
    pub reload_interval: Option<u64>,
    pub armed_by_default: bool,
}

fn validate_scoping(scoping: &Scoping, context: &str, errors: &mut Vec<String>) {
    if scoping.exclude_channels.is_some() && scoping.include_channels.is_some() {
        errors.push(format!("in {}, scoping rule specifies both exclude_channels and include_channels. Specify only one.", context));
    }

    if scoping.exclude_channels.is_some() && scoping.exclude_channels.as_ref().unwrap().is_empty() {
        errors.push(format!(
            "in {}, scoping rule specifies an empty exclude_channels; omit the key instead.",
            context
        ));
    }

    if scoping.include_channels.is_some() && scoping.include_channels.as_ref().unwrap().is_empty() {
        errors.push(format!(
            "in {}, scoping rule specifies an empty include_channels; omit the key instead.",
            context
        ));
    }

    if scoping.exclude_roles.is_some() && scoping.exclude_roles.as_ref().unwrap().is_empty() {
        errors.push(format!(
            "in {}, scoping rule specifies an empty exclude_roles; omit the key instead.",
            context
        ));
    }
}

fn validate_message_rule(
    message_rule: &MessageFilterRule,
    context: &str,
    errors: &mut Vec<String>,
) {
    match message_rule {
        MessageFilterRule::Substring { substrings } => {
            if substrings.is_match("") {
                errors.push(format!(
                    "in {}, substrings contains an empty string; this would match all messages",
                    context
                ));
            }
        }
        MessageFilterRule::Words { words } => {
            // HACK: The empty string doesn't work here, because of the structure
            // of the deserialized `words` regex. We use the letter `a`, since the
            // regex crate provides no better way to do this...
            if words.is_match("a") {
                errors.push(format!(
                    "in {}, words contains an empty string; this would match all messages",
                    context
                ));
            }
        }
        MessageFilterRule::Regex { regexes } => {
            let matches = regexes.matches("").into_iter();
            for (index, _) in matches.enumerate() {
                errors.push(format!(
                    "in {}, regex {} matches an empty string; this would match all messages",
                    context, index,
                ));
            }
        }
        _ => {}
    }
}

pub fn validate_guild_config(guild: &GuildConfig) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();

    if let Some(scoping) = &guild.default_scoping {
        validate_scoping(scoping, "default scoping", &mut errors);
    }

    let mut has_default_actions = false;
    if let Some(actions) = &guild.default_actions {
        if actions.is_empty() {
            errors.push("default_actions is specified but is empty.".to_string());
        } else {
            has_default_actions = true;
        }
    }

    if let Some(notifications) = &guild.notifications {
        if let Some(roles) = &notifications.ping_roles {
            if roles.is_empty() {
                errors.push(
                    "notification settings, ping_roles is specified but is empty; omit the key."
                        .to_string(),
                );
            }
        }
    }

    if let Some(spam) = &guild.spam {
        if let Some(scoping) = spam.scoping.as_ref() {
            validate_scoping(scoping, "spam scoping", &mut errors);
        }

        if let Some(actions) = &spam.actions {
            if actions.is_empty() {
                errors.push("in spam config, actions is specified but is empty.".to_string());
            }
        } else if !has_default_actions {
            errors.push("in spam config, no actions are specified and there are no default actions for this guild.".to_string());
        }

        if spam.emoji.is_none()
            && spam.attachments.is_none()
            && spam.duplicates.is_none()
            && spam.links.is_none()
            && spam.spoilers.is_none()
        {
            errors.push("in spam config, no spam thresholds are specified. Spam filtering will have no effects.".to_string());
        }
    }

    if let Some(usernames) = &guild.usernames {
        if usernames.actions.is_empty() {
            errors.push("in username config, actions is empty.".to_string());
        }

        if usernames.rules.is_empty() {
            errors.push("in username config, rules is empty.".to_string());
        }
    }

    if let Some(messages) = &guild.messages {
        if messages.is_empty() {
            errors.push("messages is empty; omit the key.".to_string());
        }

        for (i, filter) in messages.iter().enumerate() {
            match &filter.actions {
                Some(actions) => {
                    if actions.is_empty() {
                        errors.push(format!("message filter {} has an empty actions array; omit the key to use default actions", i));
                    }
                }
                None => {
                    if !has_default_actions {
                        errors.push(format!("message filter {} does not specify actions, but this guild has no default actions.", i));
                    }
                }
            }

            if let Some(scoping) = &filter.scoping {
                validate_scoping(scoping, &format!("message filter {}", i), &mut errors);
            }

            if filter.rules.is_empty() {
                errors.push(format!("message filter {} has no rules", i));
            } else {
                for (index, rule) in filter.rules.iter().enumerate() {
                    validate_message_rule(
                        rule,
                        &format!("message filter {}, rule {}", i, index),
                        &mut errors,
                    );
                }
            }
        }
    }

    if let Some(reactions) = &guild.reactions {
        if reactions.is_empty() {
            errors.push(
                "reactions is specified but is empty; omit the key to disable reaction filtering"
                    .to_string(),
            );
        }

        for (i, filter) in reactions.iter().enumerate() {
            match &filter.actions {
                Some(actions) => {
                    if actions.is_empty() {
                        errors.push(format!("reaction filter {} has an empty actions array; omit the key to use default actions", i));
                    }
                }
                None => {
                    if !has_default_actions {
                        errors.push(format!("reaction filter {} does not specify actions, but this guild has no default actions.", i));
                    }
                }
            }

            if let Some(scoping) = &filter.scoping {
                validate_scoping(scoping, &format!("reaction filter {}", i), &mut errors);
            }

            if filter.rules.is_empty() {
                errors.push(format!("reaction filter {} has no rules", i));
            }
        }
    }

    if !errors.is_empty() {
        Err(errors)
    } else {
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LoadConfigError {
    #[error("I/O error: {0:?}")]
    Io(#[from] std::io::Error),
    #[error("Deserialization error: {0:?}")]
    Deserialize(#[from] serde_yaml::Error),
    #[error("Configuration validation error: {0:?}")]
    Validate(Vec<String>),
}

pub fn load_config(config_root: &Path, guild_id: Id<GuildMarker>) -> Result<GuildConfig> {
    let mut config_path = config_root.join(guild_id.to_string());
    config_path.set_extension("yml");

    let config_string = std::fs::read_to_string(&config_path)
        .wrap_err(format!("Unable to read {:?}", config_path))?;
    let config_yaml = serde_yaml::from_str(&config_string)?;

    match validate_guild_config(&config_yaml) {
        Ok(()) => Ok(config_yaml),
        Err(errs) => Err(LoadConfigError::Validate(errs).into()),
    }
}

pub fn load_guild_configs(
    config_root: &Path,
    guild_ids: &[Id<GuildMarker>],
) -> Result<HashMap<Id<GuildMarker>, GuildConfig>, (Id<GuildMarker>, eyre::Report)> {
    let mut configs = HashMap::new();

    for guild_id in guild_ids {
        let guild_id = *guild_id;

        let guild_config = load_config(config_root, guild_id)
            .wrap_err(format!(
                "Unable to load configuration for guild {}",
                guild_id
            ))
            .map_err(|e| (guild_id, e))?;
        configs.insert(guild_id, guild_config);
    }

    Ok(configs)
}

pub fn load_all_guild_configs(config_root: &Path) -> Result<()> {
    for entry in std::fs::read_dir(config_root)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            let path = entry.path();
            let config_string =
                std::fs::read_to_string(&path).wrap_err(format!("Unable to read {:?}", path))?;
            let config_yaml = serde_yaml::from_str(&config_string)
                .wrap_err(format!("Unable to deserialize {:?}", path))?;

            match validate_guild_config(&config_yaml) {
                Ok(()) => {}
                Err(errs) => {
                    let err = LoadConfigError::Validate(errs);
                    let err: eyre::Report = err.into();
                    return Err(err.wrap_err(format!("Unable to validate {:?}", path)));
                }
            }
        }
    }

    Ok(())
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
            serde_yaml::from_str(&json).expect("couldn't deserialize MessageFilterRule");

        if let MessageFilterRule::Words { words } = rule {
            assert_eq!(words.to_string(), "\\b(a|b|a\\(b\\))\\b");
        } else {
            assert!(false, "deserialized wrong filter");
        }
    }

    #[test]
    fn validate_catches_empty_regex() {
        let yml = r#"
        type: substring
        substrings: []
        "#;

        let rule: MessageFilterRule =
            serde_yaml::from_str(&yml).expect("couldn't deserialize MessageFilterRule");
        let mut errors = vec![];
        super::validate_message_rule(&rule, "rule", &mut errors);
        assert_eq!(
            errors,
            vec!["in rule, substrings contains an empty string; this would match all messages"]
        );

        let yml = r#"
        type: words
        words: []
        "#;

        let rule: MessageFilterRule =
            serde_yaml::from_str(&yml).expect("couldn't deserialize MessageFilterRule");
        let mut errors = vec![];
        super::validate_message_rule(&rule, "rule", &mut errors);
        assert_eq!(
            errors,
            vec!["in rule, words contains an empty string; this would match all messages"]
        );

        let yml = r#"
        type: regex
        regexes: [""]
        "#;

        let rule: MessageFilterRule =
            serde_yaml::from_str(&yml).expect("couldn't deserialize MessageFilterRule");
        let mut errors = vec![];
        super::validate_message_rule(&rule, "rule", &mut errors);
        assert_eq!(
            errors,
            vec!["in rule, regex 0 matches an empty string; this would match all messages"]
        );
    }
}
