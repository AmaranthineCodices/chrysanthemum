use std::collections::HashMap;

use color_eyre::eyre::Result;
use twilight_embed_builder::{EmbedBuilder, EmbedFieldBuilder};
use twilight_http::Client;
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

use crate::config::SlashCommands;

#[derive(Debug)]
pub(crate) struct CommandState {
    guild_commands: HashMap<GuildId, CommandId>,
}

#[tracing::instrument("Creating slash commands")]
pub(crate) async fn create_commands_for_guild(http: &Client, guild_id: GuildId, command_config: &SlashCommands) -> Result<CommandId> {
    let cmd = http
        .create_guild_command(guild_id, "chrysanthemum")
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
                name: "arm".to_owned(),
                description: "Enables Chrysanthemum globally.".to_owned(),
                options: vec![],
            }),
            CommandOption::SubCommand(OptionsCommandOptionData {
                name: "disarm".to_owned(),
                description: "Disables Chrysanthemum globally.".to_owned(),
                options: vec![],
            }),
            CommandOption::SubCommand(OptionsCommandOptionData {
                name: "reload".to_owned(),
                description: "Reloads Chrysanthemum's guild configurations.".to_owned(),
                options: vec![],
            }),
        ])?
        .exec()
        .await?;

    let cmd = cmd.model().await?;
    Ok(cmd.id.unwrap())
}

#[tracing::instrument("Updating command permissions")]
pub async fn update_guild_command_permissions(http: &Client, guild_id: GuildId, command_config: &SlashCommands, command_id: CommandId) -> Result<()> {
    let permissions: Vec<_> = command_config
        .roles
        .iter()
        .map(|r| CommandPermissions {
            id: CommandPermissionsType::Role(*r),
            permission: true,
        })
        .collect();

    http
        .update_command_permissions(guild_id, command_id, &permissions)?
        .exec()
        .await?;

    Ok(())
}

#[tracing::instrument("Updating commands to match new configuration")]
pub async fn update_guild_commands(http: &Client, guild_id: GuildId, old_config: Option<&SlashCommands>, new_config: Option<&SlashCommands>, command_id: Option<CommandId>) -> Result<Option<CommandId>> {
    match (old_config, new_config, command_id) {
        // Permissions have potentially changed.
        (Some(old_config), Some(new_config), Some(command_id)) => {
            // We don't want to change permissions redundantly or we'll run into
            // Discord quotas on this endpoint fairly quickly.
            if old_config.roles == new_config.roles {
                return Ok(Some(command_id));
            }

            update_guild_command_permissions(http, guild_id, new_config, command_id).await?;
            Ok(Some(command_id))
        },
        // Command isn't registered.
        (Some(_), Some(new_config), None) => {
            Ok(Some(create_commands_for_guild(http, guild_id, new_config).await?))
        },
        // Need to create the commands.
        (None, Some(new_config), _) => {
            Ok(Some(create_commands_for_guild(http, guild_id, new_config).await?))
        },
        // Need to delete the commands.
        (Some(_), None, Some(command_id)) => {
            http.delete_guild_command(guild_id, command_id)?.exec().await?;
            Ok(None)
        },
        // We never registered commands for this guild, and the new config doesn't
        // need them, so do nothing.
        (Some(_), None, None) => Ok(None),
        // Do nothing in this case.
        (None, None, _) => Ok(None),
    }
}

pub(crate) async fn handle_command(
    state: crate::State,
    cmd: &ApplicationCommand,
) -> Result<()> {
    if cmd.guild_id.is_none() {
        return Ok(());
    }

    let guild_id = cmd.guild_id.unwrap();

    let expected_cmd_id = {
        let cmd_ids = state.cmd_ids.read().await;
        *cmd_ids.get(&cmd.guild_id.unwrap()).unwrap_or(&None)
    };

    if expected_cmd_id.is_none() {
        tracing::trace!("Command ID doesn't exist");
        return Ok(());
    }

    if expected_cmd_id.unwrap() != cmd.data.id {
        tracing::trace!("Unexpected command ID");
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

                let guild_cfgs = state.guild_cfgs.read().await;

                if let Some(guild_config) = guild_cfgs.get(&guild_id) {
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
        "arm" => {
            state.armed.store(true, std::sync::atomic::Ordering::Relaxed);
            state
                .http
                .interaction_callback(
                    cmd.id,
                    &cmd.token,
                    &InteractionResponse::ChannelMessageWithSource(
                        CallbackDataBuilder::new()
                            .flags(MessageFlags::EPHEMERAL)
                            .content("Chrysanthemum **armed**.".to_owned())
                            .build()
                    ),
                )
                .exec()
                .await
                .unwrap();
        },
        "disarm" => {
            state.armed.store(false, std::sync::atomic::Ordering::Relaxed);
            state
                .http
                .interaction_callback(
                    cmd.id,
                    &cmd.token,
                    &InteractionResponse::ChannelMessageWithSource(
                        CallbackDataBuilder::new()
                            .flags(MessageFlags::EPHEMERAL)
                            .content("Chrysanthemum **disarmed**.".to_owned())
                            .build()
                    ),
                )
                .exec()
                .await
                .unwrap();
        },
        "reload" => {
            let result = crate::reload_guild_configs(&state).await;
            let embed = match result {
                Ok(()) => EmbedBuilder::new().title("Reload successful").color(0x32_a8_52).build().unwrap(),
                Err(report) => {
                    let report = report.to_string();
                    EmbedBuilder::new().title("Reload failure").field(EmbedFieldBuilder::new("Reason", format!("```{}```", report)).build()).build().unwrap()
                }
            };

            state.http.interaction_callback(cmd.id, &cmd.token, &InteractionResponse::ChannelMessageWithSource(
                CallbackDataBuilder::new().flags(MessageFlags::EPHEMERAL).embeds(vec![embed]).build()
            )).exec().await.unwrap();
        }
        _ => unreachable!(),
    }

    Ok(())
}
