use std::{borrow::Cow, sync::Arc};

use tokio::sync::RwLock;
use twilight_mention::Mention as MentionTrait;
use twilight_model::channel::message::Mention;

use crate::{
    action::MessageAction,
    config::{MessageFilter, MessageFilterAction, Scoping, SpamFilter},
    filter::{check_spam_record, SpamHistory},
    model::MessageInfo,
};

const SPAM_FILTER_NAME: &str = "Spam";

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MessageFilterFailure {
    pub(crate) actions: Vec<MessageAction>,
    pub(crate) filter_name: String,
    pub(crate) context: &'static str,
}

pub(crate) fn clean_mentions(content: &str, mentions: &[Mention]) -> String {
    let mut message_content = content.to_string();

    for mention in mentions {
        let display_name = mention.member.as_ref()
            .and_then(|member| member.nick.as_deref())
            .unwrap_or(&mention.name);

        let clean_mention = format!("@{}", display_name);
        let raw_mention = mention.id.mention().to_string();

        message_content = message_content.replace(&raw_mention, &clean_mention);
    }

    message_content
}

fn format_message_preview(format_string: String, content: &str) -> String {
    const MAX_CHARS: usize = 2_000;
    const MESSAGE_PREVIEW: &str = "$MESSAGE_PREVIEW";
    const ELLIPSIS: &str = "…";

    if format_string.contains(MESSAGE_PREVIEW) {
        let available_length = MAX_CHARS - format_string.len() - MESSAGE_PREVIEW.len();
        let truncated_content = if content.len() > available_length {
            let mut last_index = available_length - ELLIPSIS.len();
            while !content.is_char_boundary(last_index) {
                last_index -= 1;
            }

            Cow::Owned(format!("{}{}", &content[0..last_index], ELLIPSIS))
        } else {
            Cow::Borrowed(content)
        };

        debug_assert!(truncated_content.len() <= available_length);
        format_string.replacen(MESSAGE_PREVIEW, &truncated_content, 1)
    } else {
        format_string
    }
}

fn map_filter_action_to_action(
    filter_action: &MessageFilterAction,
    message: &MessageInfo,
    filter_name: &str,
    filter_reason: &str,
    context: &'static str,
) -> MessageAction {
    match filter_action {
        MessageFilterAction::Delete => MessageAction::Delete {
            message_id: message.id,
            channel_id: message.channel_id,
        },
        MessageFilterAction::SendLog {
            channel_id: log_channel,
        } => MessageAction::SendLog {
            to: *log_channel,
            filter_name: filter_name.to_string(),
            message_channel: message.channel_id,
            content: message.content.to_string(),
            filter_reason: filter_reason.to_string(),
            author: message.author_id,
            context,
        },
        MessageFilterAction::SendMessage {
            channel_id,
            content,
            requires_armed,
        } => {
            let formatted_content = content.replace("$USER_ID", &message.author_id.to_string());
            let formatted_content = formatted_content.replace("$FILTER_REASON", filter_reason);

            let formatted_content = format_message_preview(formatted_content, message.content);

            MessageAction::SendMessage {
                to: *channel_id,
                content: formatted_content,
                requires_armed: *requires_armed,
            }
        }
    }
}

#[tracing::instrument(skip(filters, default_scoping, default_actions))]
fn filter_message(
    filters: &[MessageFilter],
    default_scoping: Option<&Scoping>,
    default_actions: Option<&[MessageFilterAction]>,
    message: &MessageInfo,
    context: &'static str,
) -> Result<(), MessageFilterFailure> {
    for filter in filters {
        if let Some(scoping) = filter.scoping.as_ref().or(default_scoping) {
            if !scoping.is_included(message.channel_id, message.author_roles) {
                continue;
            }
        }

        let result = filter.filter_message(message);
        if let Err(reason) = result {
            if let Some(actions) = filter.actions.as_deref().or(default_actions) {
                let actions = actions
                    .iter()
                    .map(|a| {
                        map_filter_action_to_action(a, message, &filter.name, &reason, context)
                    })
                    .collect();

                return Err(MessageFilterFailure {
                    filter_name: filter.name.clone(),
                    actions,
                    context,
                });
            } else {
                return Err(MessageFilterFailure {
                    actions: vec![],
                    filter_name: filter.name.clone(),
                    context,
                });
            }
        }
    }

    Ok(())
}

// Explicit lifetime is necessary to prevent https://github.com/rust-lang/rust/issues/63033
// from occurring. We technically want two lifetimes, 'cfg and 'msg, but that also
// triggers that issue.
#[tracing::instrument(skip(spam_config, default_scoping, default_actions, spam_history))]
async fn spam_check_message<'msg>(
    spam_config: &'msg SpamFilter,
    default_scoping: Option<&'msg Scoping>,
    default_actions: Option<&'msg [MessageFilterAction]>,
    spam_history: Arc<RwLock<SpamHistory>>,
    message: &'msg MessageInfo<'msg>,
    context: &'static str,
    now: u64,
) -> Result<(), MessageFilterFailure> {
    if let Some(scoping) = spam_config.scoping.as_ref().or(default_scoping) {
        if !scoping.is_included(message.channel_id, message.author_roles) {
            return Ok(());
        }
    }

    let result = check_spam_record(message, spam_config, spam_history, now).await;

    match result {
        Ok(()) => Ok(()),
        Err(reason) => {
            let actions = spam_config
                .actions
                .as_deref()
                .or(default_actions)
                .unwrap_or(&[])
                .iter()
                .map(|a| {
                    map_filter_action_to_action(a, message, SPAM_FILTER_NAME, &reason, context)
                })
                .collect();
            Err(MessageFilterFailure {
                actions,
                filter_name: SPAM_FILTER_NAME.to_string(),
                context,
            })
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[tracing::instrument(skip(spam_config, filters, default_scoping, default_actions, spam_history))]
pub(crate) async fn filter_and_spam_check_message<'msg>(
    spam_config: Option<&'msg SpamFilter>,
    filters: &'msg [MessageFilter],
    default_scoping: Option<&'msg Scoping>,
    default_actions: Option<&'msg [MessageFilterAction]>,
    spam_history: Arc<RwLock<SpamHistory>>,
    message: &'msg MessageInfo<'msg>,
    context: &'static str,
    now: u64,
) -> Result<(), MessageFilterFailure> {
    let result = filter_message(filters, default_scoping, default_actions, message, context);

    if let Ok(()) = result {
        if let Some(spam_config) = spam_config {
            spam_check_message(
                spam_config,
                default_scoping,
                default_actions,
                spam_history,
                message,
                context,
                now,
            )
            .await
        } else {
            Ok(())
        }
    } else {
        result
    }
}

#[cfg(test)]
mod test {
    use std::{collections::HashMap, sync::Arc};

    use pretty_assertions::assert_eq;
    use regex::Regex;
    use tokio::sync::RwLock;
    use twilight_model::id::Id;

    use twilight_mention::Mention as MentionTrait;
    use super::MessageFilterFailure;

    use crate::{
        action::MessageAction,
        config::{MessageFilter, MessageFilterAction, MessageFilterRule, Scoping, SpamFilter},
    };

    #[test]
    fn filter_basic() {
        let filters = vec![MessageFilter {
            name: "first".to_string(),
            rules: vec![MessageFilterRule::Words {
                words: Regex::new("\\b(bad)\\b").unwrap(),
            }],
            scoping: Some(Scoping {
                include_channels: Some(vec![crate::model::test::CHANNEL_ID]),
                ..Default::default()
            }),
            actions: Some(vec![
                MessageFilterAction::Delete,
                MessageFilterAction::SendMessage {
                    channel_id: Id::new(1),
                    content: "$USER_ID\n$FILTER_REASON\n$MESSAGE_PREVIEW".to_string(),
                    requires_armed: false,
                },
                MessageFilterAction::SendLog {
                    channel_id: Id::new(1),
                },
            ]),
        }];

        let message = crate::model::test::message(crate::model::test::BAD_CONTENT);
        let result = super::filter_message(&filters, None, None, &message, "message create");
        assert_eq!(
            result,
            Err(MessageFilterFailure {
                filter_name: "first".to_owned(),
                context: "message create",
                actions: vec![
                    MessageAction::Delete {
                        message_id: crate::model::test::MESSAGE_ID,
                        channel_id: crate::model::test::CHANNEL_ID,
                    },
                    MessageAction::SendMessage {
                        to: Id::new(1),
                        content: "3
contains word `bad`
asdf bad message z̷̢͈͓̥̤͕̰̤̔͒̄̂̒͋̔̀̒͑̈̅̍̐a̶̡̘̬̯̩̣̪̤̹̖͓͉̿l̷̼̬͊͊̀́̽̑̕g̵̝̗͇͇̈́̄͌̈́͊̌̋͋̑̌̕͘͘ơ̵̢̰̱̟͑̀̂͗́̈́̀  https://example.com/ discord.gg/evilserver"
                            .to_owned(),
                        requires_armed: false,
                    },
                    MessageAction::SendLog {
                        to: Id::new(1),
                        filter_name: "first".to_owned(),
                        message_channel: crate::model::test::CHANNEL_ID,
                        content: crate::model::test::BAD_CONTENT.to_owned(),
                        filter_reason: "contains word `bad`".to_owned(),
                        author: crate::model::test::USER_ID,
                        context: "message create",
                    }
                ],
            })
        )
    }

    #[test]
    fn use_default_scoping_if_no_scoping() {
        let filters = vec![MessageFilter {
            name: "first".to_string(),
            rules: vec![MessageFilterRule::Words {
                words: Regex::new("\\b(bad)\\b").unwrap(),
            }],
            scoping: None,
            actions: Some(vec![MessageFilterAction::Delete]),
        }];

        let default_scoping = Scoping {
            include_channels: Some(vec![crate::model::test::CHANNEL_ID]),
            ..Default::default()
        };

        let message = crate::model::test::message(crate::model::test::BAD_CONTENT);
        let result = super::filter_message(
            &filters,
            Some(&default_scoping),
            None,
            &message,
            "message create",
        );
        assert_eq!(
            result,
            Err(MessageFilterFailure {
                filter_name: "first".to_owned(),
                context: "message create",
                actions: vec![MessageAction::Delete {
                    message_id: crate::model::test::MESSAGE_ID,
                    channel_id: crate::model::test::CHANNEL_ID,
                }],
            })
        );
    }

    #[test]
    fn scoping_overrides_default_scoping() {
        let filters = vec![MessageFilter {
            name: "first".to_string(),
            rules: vec![MessageFilterRule::Words {
                words: Regex::new("\\b(bad)\\b").unwrap(),
            }],
            scoping: Some(Scoping {
                include_channels: Some(vec![crate::model::test::CHANNEL_ID]),
                ..Default::default()
            }),
            actions: Some(vec![MessageFilterAction::Delete]),
        }];

        let default_scoping = Scoping {
            exclude_channels: Some(vec![crate::model::test::CHANNEL_ID]),
            ..Default::default()
        };

        let message = crate::model::test::message(crate::model::test::BAD_CONTENT);
        let result = super::filter_message(
            &filters,
            Some(&default_scoping),
            None,
            &message,
            "message create",
        );
        assert_eq!(
            result,
            Err(MessageFilterFailure {
                filter_name: "first".to_owned(),
                context: "message create",
                actions: vec![MessageAction::Delete {
                    message_id: crate::model::test::MESSAGE_ID,
                    channel_id: crate::model::test::CHANNEL_ID,
                }],
            })
        );
    }

    #[test]
    fn evaluate_filters_in_order() {
        let filters = vec![
            MessageFilter {
                name: "first".to_string(),
                rules: vec![MessageFilterRule::Words {
                    words: Regex::new("\\b(bad)\\b").unwrap(),
                }],
                scoping: None,
                actions: Some(vec![MessageFilterAction::Delete]),
            },
            MessageFilter {
                name: "second".to_string(),
                rules: vec![MessageFilterRule::Words {
                    words: Regex::new("\\b(bad|special)\\b").unwrap(),
                }],
                scoping: None,
                actions: Some(vec![MessageFilterAction::Delete]),
            },
        ];

        let default_scoping = Scoping {
            include_channels: Some(vec![crate::model::test::CHANNEL_ID]),
            ..Default::default()
        };

        let message = crate::model::test::message(crate::model::test::BAD_CONTENT);
        let result = super::filter_message(
            &filters,
            Some(&default_scoping),
            None,
            &message,
            "message create",
        );
        assert_eq!(
            result,
            Err(MessageFilterFailure {
                filter_name: "first".to_owned(),
                context: "message create",
                actions: vec![MessageAction::Delete {
                    message_id: crate::model::test::MESSAGE_ID,
                    channel_id: crate::model::test::CHANNEL_ID,
                }],
            })
        );

        let second_message = crate::model::test::message("special message");
        let result = super::filter_message(
            &filters,
            Some(&default_scoping),
            None,
            &second_message,
            "message create",
        );
        assert_eq!(
            result,
            Err(MessageFilterFailure {
                filter_name: "second".to_owned(),
                context: "message create",
                actions: vec![MessageAction::Delete {
                    message_id: crate::model::test::MESSAGE_ID,
                    channel_id: crate::model::test::CHANNEL_ID,
                }],
            })
        );
    }

    #[test]
    fn use_default_actions_if_no_actions() {
        let filters = vec![MessageFilter {
            name: "first".to_string(),
            rules: vec![MessageFilterRule::Words {
                words: Regex::new("\\b(bad)\\b").unwrap(),
            }],
            scoping: Some(Scoping {
                include_channels: Some(vec![crate::model::test::CHANNEL_ID]),
                ..Default::default()
            }),
            actions: None,
        }];

        let default_actions = vec![MessageFilterAction::Delete];

        let message = crate::model::test::message(crate::model::test::BAD_CONTENT);
        let result = super::filter_message(
            &filters,
            None,
            Some(&default_actions),
            &message,
            "message create",
        );
        assert_eq!(
            result,
            Err(MessageFilterFailure {
                filter_name: "first".to_owned(),
                context: "message create",
                actions: vec![MessageAction::Delete {
                    message_id: crate::model::test::MESSAGE_ID,
                    channel_id: crate::model::test::CHANNEL_ID,
                }],
            })
        );
    }

    #[test]
    fn use_no_actions_if_none_are_specified() {
        let filters = vec![MessageFilter {
            name: "first".to_string(),
            rules: vec![MessageFilterRule::Words {
                words: Regex::new("\\b(bad)\\b").unwrap(),
            }],
            scoping: Some(Scoping {
                include_channels: Some(vec![crate::model::test::CHANNEL_ID]),
                ..Default::default()
            }),
            actions: None,
        }];

        let message = crate::model::test::message(crate::model::test::BAD_CONTENT);
        let result = super::filter_message(&filters, None, None, &message, "message create");
        assert_eq!(
            result,
            Err(MessageFilterFailure {
                filter_name: "first".to_owned(),
                context: "message create",
                actions: vec![],
            })
        );
    }

    #[test]
    fn actions_override_default_actions() {
        let filters = vec![MessageFilter {
            name: "first".to_string(),
            rules: vec![MessageFilterRule::Words {
                words: Regex::new("\\b(bad)\\b").unwrap(),
            }],
            scoping: Some(Scoping {
                include_channels: Some(vec![crate::model::test::CHANNEL_ID]),
                ..Default::default()
            }),
            actions: Some(vec![MessageFilterAction::SendMessage {
                channel_id: Id::new(2),
                content: "filtered".to_owned(),
                requires_armed: false,
            }]),
        }];

        let default_actions = vec![MessageFilterAction::Delete];

        let message = crate::model::test::message(crate::model::test::BAD_CONTENT);
        let result = super::filter_message(
            &filters,
            None,
            Some(&default_actions),
            &message,
            "message create",
        );
        assert_eq!(
            result,
            Err(MessageFilterFailure {
                filter_name: "first".to_owned(),
                context: "message create",
                actions: vec![MessageAction::SendMessage {
                    to: Id::new(2),
                    content: "filtered".to_owned(),
                    requires_armed: false,
                }],
            })
        );
    }

    #[test]
    fn pass_if_no_filters_filter() {
        let filters = vec![MessageFilter {
            name: "first".to_string(),
            rules: vec![MessageFilterRule::Words {
                words: Regex::new("\\b(bad)\\b").unwrap(),
            }],
            scoping: Some(Scoping {
                include_channels: Some(vec![crate::model::test::CHANNEL_ID]),
                ..Default::default()
            }),
            actions: Some(vec![MessageFilterAction::Delete]),
        }];

        let message = crate::model::test::message(crate::model::test::GOOD_CONTENT);
        let result = super::filter_message(&filters, None, None, &message, "message create");
        assert_eq!(result, Ok(()));
    }

    #[tokio::test]
    async fn spam_check() {
        let spam_config = SpamFilter {
            duplicates: Some(1),
            actions: Some(vec![MessageFilterAction::Delete]),
            ..Default::default()
        };

        let spam_history = Arc::new(RwLock::new(HashMap::new()));
        let message = crate::model::test::message_at_time(crate::model::test::BAD_CONTENT, 10);
        let result = super::spam_check_message(
            &spam_config,
            None,
            None,
            spam_history.clone(),
            &message,
            "message create",
            20,
        )
        .await;
        assert_eq!(result, Ok(()));

        let second_message =
            crate::model::test::message_at_time(crate::model::test::BAD_CONTENT, 30);
        let result = super::spam_check_message(
            &spam_config,
            None,
            None,
            spam_history.clone(),
            &second_message,
            "message create",
            40,
        )
        .await;
        assert_eq!(
            result,
            Err(MessageFilterFailure {
                filter_name: super::SPAM_FILTER_NAME.to_string(),
                context: "message create",
                actions: vec![MessageAction::Delete {
                    channel_id: crate::model::test::CHANNEL_ID,
                    message_id: crate::model::test::MESSAGE_ID,
                }]
            })
        );
    }

    #[tokio::test]
    async fn spam_check_use_default_scoping_if_no_scoping() {
        let spam_config = SpamFilter {
            spoilers: Some(1),
            actions: Some(vec![MessageFilterAction::Delete]),
            ..Default::default()
        };

        let default_scoping = Scoping {
            exclude_channels: Some(vec![crate::model::test::CHANNEL_ID]),
            ..Default::default()
        };

        let spam_history = Arc::new(RwLock::new(HashMap::new()));
        let message = crate::model::test::message_at_time("|| || || ||", 10);
        let result = super::spam_check_message(
            &spam_config,
            Some(&default_scoping),
            None,
            spam_history.clone(),
            &message,
            "message create",
            20,
        )
        .await;
        assert_eq!(result, Ok(()));
    }

    #[tokio::test]
    async fn spam_check_scoping_overrides_default_scoping() {
        let spam_config = SpamFilter {
            spoilers: Some(1),
            actions: Some(vec![MessageFilterAction::Delete]),
            scoping: Some(Scoping {
                include_channels: Some(vec![crate::model::test::CHANNEL_ID]),
                ..Default::default()
            }),
            ..Default::default()
        };

        let default_scoping = Scoping {
            exclude_channels: Some(vec![crate::model::test::CHANNEL_ID]),
            ..Default::default()
        };

        let spam_history = Arc::new(RwLock::new(HashMap::new()));
        let message = crate::model::test::message_at_time("|| || || ||", 10);
        let result = super::spam_check_message(
            &spam_config,
            Some(&default_scoping),
            None,
            spam_history.clone(),
            &message,
            "message create",
            20,
        )
        .await;
        assert_eq!(
            result,
            Err(MessageFilterFailure {
                filter_name: super::SPAM_FILTER_NAME.to_string(),
                context: "message create",
                actions: vec![MessageAction::Delete {
                    message_id: crate::model::test::MESSAGE_ID,
                    channel_id: crate::model::test::CHANNEL_ID,
                }]
            })
        );
    }

    #[tokio::test]
    async fn spam_check_use_default_actions_if_no_actions() {
        let spam_config = SpamFilter {
            spoilers: Some(1),
            actions: None,
            scoping: None,
            ..Default::default()
        };

        let default_actions = vec![MessageFilterAction::Delete];

        let spam_history = Arc::new(RwLock::new(HashMap::new()));
        let message = crate::model::test::message_at_time("|| || || ||", 10);
        let result = super::spam_check_message(
            &spam_config,
            None,
            Some(&default_actions),
            spam_history.clone(),
            &message,
            "message create",
            20,
        )
        .await;
        assert_eq!(
            result,
            Err(MessageFilterFailure {
                filter_name: super::SPAM_FILTER_NAME.to_string(),
                context: "message create",
                actions: vec![MessageAction::Delete {
                    message_id: crate::model::test::MESSAGE_ID,
                    channel_id: crate::model::test::CHANNEL_ID,
                }]
            })
        );
    }

    #[tokio::test]
    async fn spam_check_actions_override_default_actions() {
        let spam_config = SpamFilter {
            spoilers: Some(1),
            actions: Some(vec![MessageFilterAction::Delete]),
            scoping: None,
            ..Default::default()
        };

        let default_actions = vec![];

        let spam_history = Arc::new(RwLock::new(HashMap::new()));
        let message = crate::model::test::message_at_time("|| || || ||", 10);
        let result = super::spam_check_message(
            &spam_config,
            None,
            Some(&default_actions),
            spam_history.clone(),
            &message,
            "message create",
            20,
        )
        .await;
        assert_eq!(
            result,
            Err(MessageFilterFailure {
                filter_name: super::SPAM_FILTER_NAME.to_string(),
                context: "message create",
                actions: vec![MessageAction::Delete {
                    message_id: crate::model::test::MESSAGE_ID,
                    channel_id: crate::model::test::CHANNEL_ID,
                }]
            })
        );
    }

    #[tokio::test]
    async fn spam_check_after_filters() {
        let filters = vec![MessageFilter {
            name: "first".to_string(),
            rules: vec![MessageFilterRule::Words {
                words: Regex::new("\\b(bad)\\b").unwrap(),
            }],
            scoping: None,
            actions: Some(vec![MessageFilterAction::Delete]),
        }];

        let spam_config = SpamFilter {
            duplicates: Some(1),
            actions: Some(vec![MessageFilterAction::Delete]),
            ..Default::default()
        };

        let spam_history = Arc::new(RwLock::new(HashMap::new()));
        let message = crate::model::test::message_at_time(crate::model::test::BAD_CONTENT, 10);
        let result = super::filter_and_spam_check_message(
            Some(&spam_config),
            &filters,
            None,
            None,
            spam_history.clone(),
            &message,
            "message create",
            20,
        )
        .await;
        assert_eq!(
            result,
            Err(MessageFilterFailure {
                filter_name: "first".to_string(),
                context: "message create",
                actions: vec![MessageAction::Delete {
                    message_id: crate::model::test::MESSAGE_ID,
                    channel_id: crate::model::test::CHANNEL_ID,
                }]
            })
        );

        let second_message =
            crate::model::test::message_at_time(crate::model::test::BAD_CONTENT, 30);
        let result = super::filter_and_spam_check_message(
            Some(&spam_config),
            &filters,
            None,
            None,
            spam_history.clone(),
            &second_message,
            "message create",
            40,
        )
        .await;
        assert_eq!(
            result,
            Err(MessageFilterFailure {
                filter_name: "first".to_string(),
                context: "message create",
                actions: vec![MessageAction::Delete {
                    message_id: crate::model::test::MESSAGE_ID,
                    channel_id: crate::model::test::CHANNEL_ID,
                }]
            })
        );
    }

    #[test]
    fn clean_message_mentions() {
        let mention = crate::model::test::mention();
        let name = mention.name.clone();

        let result =
            super::clean_mentions(&format!("Hey {}", mention.id.mention()), &[mention]);

        assert_eq!(result, format!("Hey @{}", name));
    }
}