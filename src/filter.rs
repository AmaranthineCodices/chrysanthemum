use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use twilight_model::channel::{ReactionType, message::Message};
use twilight_model::id::{ChannelId, RoleId, UserId};

use once_cell::sync::OnceCell;
use regex::Regex;
use tokio::sync::RwLock;

use crate::config;

static ZALGO_REGEX: OnceCell<Regex> = OnceCell::new();
static INVITE_REGEX: OnceCell<Regex> = OnceCell::new();
static LINK_REGEX: OnceCell<Regex> = OnceCell::new();
static SPOILER_REGEX: OnceCell<Regex> = OnceCell::new();
static EMOJI_REGEX: OnceCell<Regex> = OnceCell::new();
static CUSTOM_EMOJI_REGEX: OnceCell<Regex> = OnceCell::new();
static MENTION_REGEX: OnceCell<Regex> = OnceCell::new();

pub fn init_globals() {
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
    let _ = MENTION_REGEX.set(Regex::new("<@[!&]?\\d+>").unwrap());
}

pub type FilterResult = Result<(), String>;

fn filter_values<T, V, I>(
    mode: &config::FilterMode,
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
        config::FilterMode::AllowList => values
            // Note: We use iter().any() instead of contains because we
            // sometimes pass Vec<String> as filter_values, where T is &str -
            // contains isn't smart enough to handle this case.
            .find(|v| !filter_values.iter().any(|f| f == v))
            .map(|v| Err(format!("contains unallowed {} `{}`", context, v))),
        config::FilterMode::DenyList => values
            .find(|v| filter_values.iter().any(|f| f == v))
            .map(|v| Err(format!("contains denied {} `{}`", context, v))),
    };

    result.unwrap_or(Ok(()))
}

impl config::Scoping {
    pub fn is_included(&self, channel: ChannelId, author_roles: &Vec<RoleId>) -> bool {
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

impl config::MessageFilter {
    pub fn filter_message(&self, message: &Message) -> FilterResult {
        self.rules
            .iter()
            .map(|f| f.filter_message(&message))
            .find(|r| r.is_err())
            .unwrap_or(Ok(()))
    }

    pub fn filter_text(&self, text: &str) -> FilterResult {
        self.rules
            .iter()
            .map(|f| f.filter_text(text))
            .find(|r| r.is_err())
            .unwrap_or(Ok(()))
    }
}

impl config::MessageFilterRule {
    pub fn filter_text(&self, text: &str) -> FilterResult {
        match self {
            config::MessageFilterRule::Words { words } => {
                let skeleton = crate::confusable::skeletonize(text);

                if let Some(captures) = words.captures(&skeleton) {
                    Err(format!(
                        "contains word `{}`",
                        captures.get(1).unwrap().as_str()
                    ))
                } else {
                    Ok(())
                }
            }
            config::MessageFilterRule::Substring { substrings } => {
                let skeleton = crate::confusable::skeletonize(text);

                if let Some(captures) = substrings.captures(&skeleton) {
                    Err(format!(
                        "contains substring `{}`",
                        captures.get(0).unwrap().as_str()
                    ))
                } else {
                    Ok(())
                }
            },
            config::MessageFilterRule::Regex { regexes } => {
                let skeleton = crate::confusable::skeletonize(text);

                for regex in regexes {
                    if regex.is_match(&skeleton) {
                        return Err(format!("matches regex `{}`", regex));
                    }
                }

                Ok(())
            }
            config::MessageFilterRule::Zalgo => {
                let zalgo_regex = ZALGO_REGEX.get().unwrap();
                if zalgo_regex.is_match(text) {
                    Err("contains zalgo".to_owned())
                } else {
                    Ok(())
                }
            }
            config::MessageFilterRule::Invite { mode, invites } => {
                let invite_regex = INVITE_REGEX.get().unwrap();
                let mut invite_ids = invite_regex
                    .captures_iter(text)
                    .map(|c| c.get(1).unwrap().as_str());
                filter_values(mode, "invite", &mut invite_ids, invites)
            }
            config::MessageFilterRule::Link { mode, domains } => {
                let link_regex = LINK_REGEX.get().unwrap();
                let mut link_domains = link_regex
                    .captures_iter(text)
                    .map(|c| c.get(1).unwrap().as_str())
                    // Invites should be handled separately.
                    .filter(|v| (*v) != "discord.gg");
                filter_values(mode, "domain", &mut link_domains, domains)
            }
            config::MessageFilterRule::EmojiName { names } => {
                for capture in CUSTOM_EMOJI_REGEX.get().unwrap().captures_iter(text) {
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
            _ => Ok(()),
        }
    }

    pub fn filter_message(&self, message: &Message) -> FilterResult {
        match self {
            config::MessageFilterRule::MimeType {
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
            config::MessageFilterRule::StickerId { mode, stickers } => {
                filter_values(
                    mode,
                    "sticker",
                    &mut message.sticker_items.iter().map(|s| s.id),
                    stickers,
                )
            }
            config::MessageFilterRule::StickerName { stickers } => {
                for sticker in &message.sticker_items {
                    let substring_match = stickers.captures_iter(&sticker.name).nth(0);
                    if let Some(substring_match) = substring_match {
                        return Err(format!(
                            "contains sticker with denied name substring `{}`",
                            substring_match.get(0).unwrap().as_str()
                        ));
                    }
                }

                Ok(())
            }
            _ => self.filter_text(&message.content),
        }
    }
}

impl config::ReactionFilter {
    pub fn filter_reaction(&self, reaction: &ReactionType) -> FilterResult {
        self.rules
            .iter()
            .map(|f| f.filter_reaction(&reaction))
            .find(|r| r.is_err())
            .unwrap_or(Ok(()))
    }
}

impl config::ReactionFilterRule {
    pub fn filter_reaction(&self, reaction: &ReactionType) -> FilterResult {
        match self {
            config::ReactionFilterRule::Default {
                emoji: filtered_emoji,
                mode,
            } => {
                if let ReactionType::Unicode { name } = reaction {
                    match mode {
                        config::FilterMode::AllowList => {
                            if !filtered_emoji.contains(&name) {
                                Err(format!("reacted with unallowed emoji {}", name))
                            } else {
                                Ok(())
                            }
                        }
                        config::FilterMode::DenyList => {
                            if filtered_emoji.contains(&name) {
                                Err(format!("reacted with denied emoji {}", name))
                            } else {
                                Ok(())
                            }
                        }
                    }
                } else {
                    Ok(())
                }
            }
            config::ReactionFilterRule::CustomId {
                emoji: filtered_emoji,
                mode,
            } => {
                if let ReactionType::Custom { id, .. } = reaction {
                    match mode {
                        config::FilterMode::AllowList => {
                            if !filtered_emoji.contains(&id) {
                                Err(format!("reacted with unallowed emoji {}", id))
                            } else {
                                Ok(())
                            }
                        }
                        config::FilterMode::DenyList => {
                            if filtered_emoji.contains(&id) {
                                Err(format!("reacted with denied emoji {}", id))
                            } else {
                                Ok(())
                            }
                        }
                    }
                } else {
                    Ok(())
                }
            }
            config::ReactionFilterRule::CustomName { names } => {
                if let ReactionType::Custom {
                    name: Some(name), ..
                } = reaction
                {
                    if names.is_match(&name) {
                        Err(format!("reacted with denied emoji name {}", name))
                    } else {
                        Ok(())
                    }
                } else {
                    Ok(())
                }
            }
        }
    }
}

#[derive(Debug)]
pub struct SpamRecord {
    content: String,
    emoji: u8,
    links: u8,
    attachments: u8,
    spoilers: u8,
    mentions: u8,
    sent_at: u64,
}

impl SpamRecord {
    pub fn from_message(message: &Message) -> SpamRecord {
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
        let mentions = MENTION_REGEX
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
            mentions: mentions as u8,
            sent_at: message.timestamp.as_micros(),
        }
    }
}

pub type SpamHistory = HashMap<UserId, Arc<Mutex<VecDeque<SpamRecord>>>>;

fn exceeds_spam_thresholds(
    history: &VecDeque<SpamRecord>,
    current_record: &SpamRecord,
    config: &config::SpamFilter,
) -> FilterResult {
    let (emoji_sum, link_sum, attachment_sum, spoiler_sum, mention_sum, matching_duplicates) =
        history
            .iter()
            // Start with a value of 1 for matching_duplicates because the current spam record
            // is always a duplicate of itself.
            .fold(
                (
                    current_record.emoji,
                    current_record.links,
                    current_record.attachments,
                    current_record.spoilers,
                    current_record.mentions,
                    1u8,
                ),
                |(
                    total_emoji,
                    total_links,
                    total_attachments,
                    total_spoilers,
                    total_mentions,
                    total_duplicates,
                ),
                 record| {
                    (
                        total_emoji.saturating_add(record.emoji),
                        total_links.saturating_add(record.links),
                        total_attachments.saturating_add(record.attachments),
                        total_spoilers.saturating_add(record.spoilers),
                        total_mentions.saturating_add(record.mentions),
                        total_duplicates
                            .saturating_add((record.content == current_record.content) as u8),
                    )
                },
            );

    tracing::trace!(
        "Spam summary: {} emoji, {} links, {} attachments, {} spoilers, {} mentions, {} duplicates",
        emoji_sum,
        link_sum,
        attachment_sum,
        spoiler_sum,
        mention_sum,
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
    } else if config.mentions.is_some()
        && mention_sum > config.mentions.unwrap()
        && current_record.mentions > 0
    {
        Err("sent too many mentions".to_owned())
    } else if config.duplicates.is_some() && matching_duplicates > config.duplicates.unwrap() {
        Err("sent too many duplicate messages".to_owned())
    } else {
        Ok(())
    }
}

pub async fn check_spam_record(
    message: &Message,
    config: &config::SpamFilter,
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

    let now = (Utc::now().timestamp_millis() as u64) * 1000;
    let mut cleared_count = 0;
    loop {
        match spam_history.front() {
            Some(front) => {
                if now - front.sent_at > (config.interval as u64) * 1_000_000 {
                    spam_history.pop_front();
                    cleared_count += 1;
                } else {
                    break;
                }
            }
            None => break,
        }
    }

    tracing::trace!(
        "Cleared {} spam records for user {}",
        cleared_count,
        message.author.id
    );

    let result = exceeds_spam_thresholds(&spam_history, &new_spam_record, &config);
    spam_history.push_back(new_spam_record);
    result
}
