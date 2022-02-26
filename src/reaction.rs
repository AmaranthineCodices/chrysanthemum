use crate::{
    action::ReactionAction,
    config::{MessageFilterAction, ReactionFilter, Scoping},
    model::ReactionInfo,
};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ReactionFilterFailure {
    pub(crate) filter_name: String,
    pub(crate) actions: Vec<ReactionAction>,
}

fn map_filter_action_to_action(
    filter_action: &MessageFilterAction,
    reaction: &ReactionInfo,
    filter_name: &str,
    filter_reason: &str,
) -> ReactionAction {
    match filter_action {
        MessageFilterAction::Delete => ReactionAction::Delete {
            message_id: reaction.message_id,
            channel_id: reaction.channel_id,
            reaction: reaction.reaction.clone(),
        },
        MessageFilterAction::SendMessage {
            channel_id,
            content,
            requires_armed,
        } => {
            let formatted_content = content.replace("$USER_ID", &reaction.author_id.to_string());
            let formatted_content = formatted_content.replace("$FILTER_REASON", filter_reason);

            ReactionAction::SendMessage {
                to: *channel_id,
                content: formatted_content,
                requires_armed: *requires_armed,
            }
        }
        MessageFilterAction::SendLog { channel_id } => ReactionAction::SendLog {
            to: *channel_id,
            filter_name: filter_name.to_string(),
            message: reaction.message_id,
            channel: reaction.channel_id,
            author: reaction.author_id,
            filter_reason: filter_reason.to_string(),
            reaction: reaction.reaction.clone(),
        },
    }
}

pub(crate) fn filter_reaction(
    filters: &[ReactionFilter],
    default_scoping: Option<&Scoping>,
    default_actions: Option<&[MessageFilterAction]>,
    reaction: &ReactionInfo,
) -> Result<(), ReactionFilterFailure> {
    for filter in filters {
        if let Some(scoping) = filter.scoping.as_ref().or(default_scoping) {
            if !scoping.is_included(reaction.channel_id, reaction.author_roles) {
                continue;
            }
        }

        if let Err(reason) = filter.filter_reaction(&reaction.reaction) {
            let actions = filter
                .actions
                .as_deref()
                .or(default_actions)
                .unwrap_or(&[])
                .iter()
                .map(|a| map_filter_action_to_action(a, reaction, &filter.name, &reason))
                .collect();

            return Err(ReactionFilterFailure {
                filter_name: filter.name.to_string(),
                actions,
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod test {
    use pretty_assertions::assert_eq;
    use twilight_model::id::ChannelId;

    use crate::{config::{ReactionFilter, FilterMode, MessageFilterAction, ReactionFilterRule, Scoping}, reaction::ReactionFilterFailure, action::ReactionAction};

    #[test]
    fn filter_basic() {
        let filters = vec![
            ReactionFilter {
                name: "first".to_string(),
                rules: vec![
                    ReactionFilterRule::Default {
                        mode: FilterMode::DenyList,
                        emoji: vec!["🍆".to_string()],
                    }
                ],
                scoping: None,
                actions: Some(vec![
                    MessageFilterAction::Delete,
                    MessageFilterAction::SendLog {
                        channel_id: ChannelId::new(3).unwrap(),
                    },
                    MessageFilterAction::SendMessage {
                        channel_id: ChannelId::new(3).unwrap(),
                        content: "$USER_ID $FILTER_REASON".to_string(),
                        requires_armed: false,
                    },
                ])
            },
        ];

        let rxn = crate::model::test::default_reaction("🍆");
        let result = super::filter_reaction(&filters, None, None, &rxn);
        assert_eq!(result, Err(ReactionFilterFailure {
            filter_name: "first".to_string(),
            actions: vec![
                ReactionAction::Delete {
                    message_id: crate::model::test::MESSAGE_ID,
                    channel_id: crate::model::test::CHANNEL_ID,
                    reaction: rxn.reaction.clone(),
                },
                ReactionAction::SendLog {
                    to: ChannelId::new(3).unwrap(),
                    filter_name: "first".to_string(),
                    message: crate::model::test::MESSAGE_ID,
                    channel: crate::model::test::CHANNEL_ID,
                    filter_reason: "reacted with denied emoji 🍆".to_string(),
                    author: crate::model::test::USER_ID,
                    reaction: rxn.reaction.clone(),
                },
                ReactionAction::SendMessage {
                    to: ChannelId::new(3).unwrap(),
                    content: "3 reacted with denied emoji 🍆".to_string(),
                    requires_armed: false,
                },
            ]
        }));
    }

    #[test]
    fn use_default_scoping_if_no_scoping() {
        let filters = vec![
            ReactionFilter {
                name: "first".to_string(),
                rules: vec![
                    ReactionFilterRule::Default {
                        mode: FilterMode::DenyList,
                        emoji: vec!["🍆".to_string()],
                    }
                ],
                scoping: None,
                actions: Some(vec![
                    MessageFilterAction::Delete,
                ])
            },
        ];

        let default_scoping = Scoping {
            exclude_channels: Some(vec![
                crate::model::test::CHANNEL_ID,
            ]),
            ..Default::default()
        };

        let rxn = crate::model::test::default_reaction("🍆");
        let result = super::filter_reaction(&filters, Some(&default_scoping), None, &rxn);
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn scoping_overrides_default_scoping() {
        let filters = vec![
            ReactionFilter {
                name: "first".to_string(),
                rules: vec![
                    ReactionFilterRule::Default {
                        mode: FilterMode::DenyList,
                        emoji: vec!["🍆".to_string()],
                    }
                ],
                scoping: Some(Scoping {
                    include_channels: Some(vec![
                        crate::model::test::CHANNEL_ID,
                    ]),
                    ..Default::default()
                }),
                actions: Some(vec![
                    MessageFilterAction::Delete,
                ])
            },
        ];

        let default_scoping = Scoping {
            exclude_channels: Some(vec![
                crate::model::test::CHANNEL_ID,
            ]),
            ..Default::default()
        };

        let rxn = crate::model::test::default_reaction("🍆");
        let result = super::filter_reaction(&filters, Some(&default_scoping), None, &rxn);
        assert_eq!(result, Err(ReactionFilterFailure {
            filter_name: "first".to_string(),
            actions: vec![
                ReactionAction::Delete {
                    message_id: crate::model::test::MESSAGE_ID,
                    channel_id: crate::model::test::CHANNEL_ID,
                    reaction: rxn.reaction.clone(),
                }
            ]
        }));
    }

    #[test]
    fn use_default_actions_if_no_actions() {
        let filters = vec![
            ReactionFilter {
                name: "first".to_string(),
                rules: vec![
                    ReactionFilterRule::Default {
                        mode: FilterMode::DenyList,
                        emoji: vec!["🍆".to_string()],
                    }
                ],
                scoping: None,
                actions: None,
            },
        ];

        let default_actions = vec![
            MessageFilterAction::Delete,
        ];

        let rxn = crate::model::test::default_reaction("🍆");
        let result = super::filter_reaction(&filters, None, Some(&default_actions), &rxn);
        assert_eq!(result, Err(ReactionFilterFailure {
            filter_name: "first".to_string(),
            actions: vec![
                ReactionAction::Delete {
                    message_id: crate::model::test::MESSAGE_ID,
                    channel_id: crate::model::test::CHANNEL_ID,
                    reaction: rxn.reaction.clone(),
                }
            ]
        }));
    }

    #[test]
    fn actions_override_default_actions() {
        let filters = vec![
            ReactionFilter {
                name: "first".to_string(),
                rules: vec![
                    ReactionFilterRule::Default {
                        mode: FilterMode::DenyList,
                        emoji: vec!["🍆".to_string()],
                    }
                ],
                scoping: None,
                actions: Some(vec![
                    MessageFilterAction::Delete,
                ]),
            },
        ];

        let default_actions = vec![
            MessageFilterAction::SendLog {
                channel_id: ChannelId::new(2).unwrap(),
            },
        ];

        let rxn = crate::model::test::default_reaction("🍆");
        let result = super::filter_reaction(&filters, None, Some(&default_actions), &rxn);
        assert_eq!(result, Err(ReactionFilterFailure {
            filter_name: "first".to_string(),
            actions: vec![
                ReactionAction::Delete {
                    message_id: crate::model::test::MESSAGE_ID,
                    channel_id: crate::model::test::CHANNEL_ID,
                    reaction: rxn.reaction.clone(),
                }
            ]
        }));
    }

    #[test]
    fn evaluate_filters_in_order() {
        let filters = vec![
            ReactionFilter {
                name: "first".to_string(),
                rules: vec![
                    ReactionFilterRule::Default {
                        mode: FilterMode::DenyList,
                        emoji: vec!["🍆".to_string()],
                    }
                ],
                scoping: None,
                actions: Some(vec![
                    MessageFilterAction::Delete,
                ]),
            },
            ReactionFilter {
                name: "second".to_string(),
                rules: vec![
                    ReactionFilterRule::Default {
                        mode: FilterMode::DenyList,
                        emoji: vec!["🍆".to_string(), "💜".to_string()],
                    }
                ],
                scoping: None,
                actions: Some(vec![
                    MessageFilterAction::Delete,
                ]),
            },
        ];

        let rxn = crate::model::test::default_reaction("🍆");
        let result = super::filter_reaction(&filters, None, None, &rxn);
        assert_eq!(result, Err(ReactionFilterFailure {
            filter_name: "first".to_string(),
            actions: vec![
                ReactionAction::Delete {
                    message_id: crate::model::test::MESSAGE_ID,
                    channel_id: crate::model::test::CHANNEL_ID,
                    reaction: rxn.reaction.clone(),
                }
            ]
        }));

        let rxn = crate::model::test::default_reaction("💜");
        let result = super::filter_reaction(&filters, None, None, &rxn);
        assert_eq!(result, Err(ReactionFilterFailure {
            filter_name: "second".to_string(),
            actions: vec![
                ReactionAction::Delete {
                    message_id: crate::model::test::MESSAGE_ID,
                    channel_id: crate::model::test::CHANNEL_ID,
                    reaction: rxn.reaction.clone(),
                }
            ]
        }));
    }

    #[test]
    fn use_no_actions_if_none_are_specified() {
        let filters = vec![
            ReactionFilter {
                name: "first".to_string(),
                rules: vec![
                    ReactionFilterRule::Default {
                        mode: FilterMode::DenyList,
                        emoji: vec!["🍆".to_string()],
                    }
                ],
                scoping: None,
                actions: None,
            },
        ];

        let rxn = crate::model::test::default_reaction("🍆");
        let result = super::filter_reaction(&filters, None, None, &rxn);
        assert_eq!(result, Err(ReactionFilterFailure {
            filter_name: "first".to_string(),
            actions: vec![]
        }));
    }

    #[test]
    fn pass_if_no_filters_filter() {
        let filters = vec![
            ReactionFilter {
                name: "first".to_string(),
                rules: vec![
                    ReactionFilterRule::Default {
                        mode: FilterMode::DenyList,
                        emoji: vec!["🍆".to_string()],
                    }
                ],
                scoping: None,
                actions: None,
            },
        ];

        let rxn = crate::model::test::default_reaction("💜");
        let result = super::filter_reaction(&filters, None, None, &rxn);
        assert_eq!(result, Ok(()));
    }
}
