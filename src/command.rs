use std::collections::HashMap;

use color_eyre::eyre::Result;
use twilight_embed_builder::{EmbedBuilder, EmbedFieldBuilder};
use twilight_model::{
    application::{
        callback::InteractionResponse,
        command::{
            permissions::{CommandPermissions, CommandPermissionsType},
            ChoiceCommandOptionData, CommandOption, OptionsCommandOptionData,
        },
        interaction::{application_command::CommandOptionValue, ApplicationCommand},
    },
    channel::message::MessageFlags,
    id::{CommandId, GuildId},
};
use twilight_util::builder::CallbackDataBuilder;

#[derive(Debug)]
pub(crate) struct CommandState {
    guild_commands: HashMap<GuildId, CommandId>,
}

#[tracing::instrument("Creating slash commands")]
pub(crate) async fn create_commands(state: crate::State) -> Result<CommandState> {
    let mut cmd_state = CommandState {
        guild_commands: HashMap::new(),
    };

    for (guild, cfg) in &state.cfg.guilds {
        if let Some(slash_cmds) = &cfg.slash_commands {
            let cmd = state
                .http
                .create_guild_command(*guild, "chrysanthemum")
                .unwrap()
                .chat_input("Interact with the Chrysanthemum bot")
                .unwrap()
                .default_permission(false)
                .command_options(&[
                    CommandOption::SubCommand(OptionsCommandOptionData {
                        name: "test".to_owned(),
                        description: "Tests a message against Chrysanthemum's filter.".to_owned(),
                        options: vec![CommandOption::String(ChoiceCommandOptionData {
                            name: "message".to_owned(),
                            description: "The message to test.".to_owned(),
                            required: true,
                            choices: vec![],
                        })],
                    }),
                    CommandOption::SubCommand(OptionsCommandOptionData {
                        name: "stats".to_owned(),
                        description: "Displays stats for this guild.".to_owned(),
                        options: vec![],
                    }),
                ])?
                .exec()
                .await?;

            let cmd = cmd.model().await?;

            let permissions: Vec<_> = slash_cmds
                .roles
                .iter()
                .map(|r| CommandPermissions {
                    id: CommandPermissionsType::Role(*r),
                    permission: true,
                })
                .collect();

            state
                .http
                .update_command_permissions(*guild, cmd.id.unwrap(), &permissions)?
                .exec()
                .await?;
            cmd_state.guild_commands.insert(*guild, cmd.id.unwrap());
        }
    }

    Ok(cmd_state)
}

pub(crate) async fn handle_command(
    state: crate::State,
    cmd: &ApplicationCommand,
) -> Result<()> {
    if cmd.guild_id.is_none() {
        return Ok(());
    }

    let guild_id = cmd.guild_id.unwrap();

    let cmd_state = state.cmd_state.read().await;
    if cmd_state.is_none() {
        return Ok(());
    }

    let cmd_state = cmd_state.as_ref().unwrap();
    let expected_cmd_id = cmd_state.guild_commands.get(&cmd.guild_id.unwrap());

    if expected_cmd_id.is_none() {
        return Ok(());
    }

    if expected_cmd_id.map(|v| *v).unwrap() != cmd.data.id {
        return Ok(());
    }

    if cmd.data.options.len() < 1 {
        return Ok(());
    }

    match &cmd.data.options[0].name[..] {
        "test" => {
            if let CommandOptionValue::SubCommand(options) = &cmd.data.options[0].value {
                let message_option = &options[0];

                let message = match &message_option.value {
                    CommandOptionValue::String(s) => s,
                    _ => return Ok(()),
                };

                if let Some(guild_config) = state.cfg.guilds.get(&guild_id) {
                    if let Some(message_filters) = &guild_config.messages {
                        for filter in message_filters {
                            let result = filter.filter_text(&message[..]);

                            let result_string = match result {
                                Ok(()) => "✅ Passed all filters".to_owned(),
                                Err(reason) => format!("❎ Failed filter: {}", reason),
                            };

                            state
                                .http
                                .interaction_callback(
                                    cmd.id,
                                    &cmd.token,
                                    &InteractionResponse::ChannelMessageWithSource(
                                        CallbackDataBuilder::new()
                                            .flags(MessageFlags::EPHEMERAL)
                                            .embeds(vec![EmbedBuilder::new()
                                                .title("Test filter")
                                                .field(
                                                    EmbedFieldBuilder::new(
                                                        "Input",
                                                        format!("```{}```", message),
                                                    )
                                                    .build(),
                                                )
                                                .field(
                                                    EmbedFieldBuilder::new("Result", result_string)
                                                        .build(),
                                                )
                                                .build()
                                                .unwrap()])
                                            .build(),
                                    ),
                                )
                                .exec()
                                .await
                                .unwrap();
                        }
                    }
                }
            }
        }
        "stats" => {
            let stats = state.stats.read().await;
            let stats = stats.get(&guild_id);

            if let Some(stats) = stats {
                let (filtered_messages, filtered_reactions, filtered_usernames) = {
                    let stats = stats.lock().unwrap();
                    let filtered_messages = stats.filtered_messages.to_string();
                    let filtered_reactions = stats.filtered_reactions.to_string();
                    let filtered_usernames = stats.filtered_usernames.to_string();
                    (filtered_messages, filtered_reactions, filtered_usernames)
                };

                state
                    .http
                    .interaction_callback(
                        cmd.id,
                        &cmd.token,
                        &InteractionResponse::ChannelMessageWithSource(
                            CallbackDataBuilder::new()
                                .flags(MessageFlags::EPHEMERAL)
                                .embeds(vec![EmbedBuilder::new()
                                    .title("Chrysanthemum Statistics")
                                    .field(
                                        EmbedFieldBuilder::new(
                                            "Filtered Messages",
                                            filtered_messages,
                                        )
                                        .build(),
                                    )
                                    .field(
                                        EmbedFieldBuilder::new(
                                            "Filtered Reactions",
                                            filtered_reactions,
                                        )
                                        .build(),
                                    )
                                    .field(
                                        EmbedFieldBuilder::new(
                                            "Filtered Usernames",
                                            filtered_usernames,
                                        )
                                        .build(),
                                    )
                                    .build()
                                    .unwrap()])
                                .build(),
                        ),
                    )
                    .exec()
                    .await
                    .unwrap();
            }
        }
        _ => unreachable!(),
    }

    Ok(())
}
