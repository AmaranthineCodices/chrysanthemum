use std::collections::HashMap;

use color_eyre::eyre::Result;
use twilight_http::client::InteractionClient;
use twilight_model::{
    application::{
        command::{
            permissions::{CommandPermissions, CommandPermissionsType},
            ChoiceCommandOptionData, CommandOption,
        },
        interaction::{application_command::CommandOptionValue, ApplicationCommand},
    },
    channel::message::MessageFlags,
    guild::Permissions,
    http::interaction::{InteractionResponse, InteractionResponseType},
    id::{
        marker::{CommandMarker, GuildMarker},
        Id,
    },
};
use twilight_util::builder::{
    embed::{EmbedBuilder, EmbedFieldBuilder},
    InteractionResponseDataBuilder,
};

use crate::config::{SlashCommand, SlashCommands};

#[derive(Hash, Debug, PartialEq, Eq, Clone, Copy)]
enum CommandKind {
    Test,
    Arm,
    Disarm,
    Reload,
}

impl CommandKind {
    fn get_config<'cfg>(&self, config: &'cfg SlashCommands) -> &'cfg SlashCommand {
        match self {
            CommandKind::Test => &config.test,
            CommandKind::Arm => &config.arm,
            CommandKind::Disarm => &config.disarm,
            CommandKind::Reload => &config.reload,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CommandState {
    cmds: HashMap<CommandKind, Id<CommandMarker>>,
}

impl CommandState {
    fn get_command_kind(&self, id: Id<CommandMarker>) -> Option<CommandKind> {
        for (kind, kind_id) in &self.cmds {
            if id == *kind_id {
                return Some(*kind);
            }
        }

        None
    }
}

#[tracing::instrument(skip(http, command_config))]
async fn update_command_permission(
    http: &InteractionClient<'_>,
    guild_id: Id<GuildMarker>,
    command_id: Id<CommandMarker>,
    command_config: &SlashCommand,
) -> Result<()> {
    let permissions: Vec<_> = command_config
        .roles
        .iter()
        .map(|r| CommandPermissions {
            id: CommandPermissionsType::Role(*r),
            permission: true,
        })
        .chain(command_config.users.iter().map(|u| CommandPermissions {
            id: CommandPermissionsType::User(*u),
            permission: true,
        }))
        .collect();

    http.update_command_permissions(guild_id, command_id, &permissions)?
        .exec()
        .await?;
    Ok(())
}

#[tracing::instrument(skip(http, command_config))]
pub(crate) async fn create_commands_for_guild(
    http: &InteractionClient<'_>,
    guild_id: Id<GuildMarker>,
    command_config: &SlashCommands,
) -> Result<CommandState> {
    let test_cmd = http
        .create_guild_command(guild_id)
        .chat_input(
            "chrysanthemum-test",
            "Test a message against Chrysanthemum's filter.",
        )?
        .default_member_permissions(Permissions::ADMINISTRATOR)
        .command_options(&[CommandOption::String(ChoiceCommandOptionData {
            autocomplete: false,
            name: "message".to_owned(),
            description: "The message to test.".to_owned(),
            required: true,
            choices: vec![],
            ..Default::default()
        })])?
        .exec()
        .await?
        .model()
        .await?;

    let arm_cmd = http
        .create_guild_command(guild_id)
        .chat_input("chrysanthemum-arm", "Arms Chrysanthemum.")?
        .default_member_permissions(Permissions::ADMINISTRATOR)
        .exec()
        .await?
        .model()
        .await?;

    let disarm_cmd = http
        .create_guild_command(guild_id)
        .chat_input("chrysanthemum-disarm", "Disarms Chrysanthemum.")?
        .default_member_permissions(Permissions::ADMINISTRATOR)
        .exec()
        .await?
        .model()
        .await?;

    let reload_cmd = http
        .create_guild_command(guild_id)
        .chat_input(
            "chrysanthemum-reload",
            "Reloads Chrysanthemum configurations from disk.",
        )?
        .default_member_permissions(Permissions::ADMINISTRATOR)
        .exec()
        .await?
        .model()
        .await?;

    let test_cmd = test_cmd.id.unwrap();
    let arm_cmd = arm_cmd.id.unwrap();
    let disarm_cmd = disarm_cmd.id.unwrap();
    let reload_cmd = reload_cmd.id.unwrap();

    update_command_permission(http, guild_id, arm_cmd, &command_config.arm).await?;
    update_command_permission(http, guild_id, disarm_cmd, &command_config.disarm).await?;
    update_command_permission(http, guild_id, reload_cmd, &command_config.reload).await?;
    update_command_permission(http, guild_id, test_cmd, &command_config.test).await?;

    let mut map = HashMap::new();
    map.insert(CommandKind::Arm, arm_cmd);
    map.insert(CommandKind::Disarm, disarm_cmd);
    map.insert(CommandKind::Test, test_cmd);
    map.insert(CommandKind::Reload, reload_cmd);

    Ok(CommandState { cmds: map })
}

#[tracing::instrument(skip(http, old_config, new_config, command_state))]
pub(crate) async fn update_guild_commands(
    http: &InteractionClient<'_>,
    guild_id: Id<GuildMarker>,
    old_config: Option<&SlashCommands>,
    new_config: Option<&SlashCommands>,
    command_state: Option<CommandState>,
) -> Result<Option<CommandState>> {
    match (old_config, new_config, command_state) {
        // Permissions have potentially changed.
        (Some(old_config), Some(new_config), Some(command_state)) => {
            for (kind, id) in &command_state.cmds {
                let old_config = kind.get_config(old_config);
                let new_config = kind.get_config(new_config);

                // We don't want to change permissions redundantly or we'll run into
                // Discord quotas on this endpoint fairly quickly.
                if old_config != new_config {
                    update_command_permission(http, guild_id, *id, new_config).await?;
                }
            }

            Ok(Some(command_state))
        }
        // Command isn't registered.
        (Some(_), Some(new_config), None) => Ok(Some(
            create_commands_for_guild(http, guild_id, new_config).await?,
        )),
        // Need to create the commands.
        (None, Some(new_config), _) => Ok(Some(
            create_commands_for_guild(http, guild_id, new_config).await?,
        )),
        // Need to delete the commands.
        (Some(_), None, Some(command_state)) => {
            for (_kind, id) in &command_state.cmds {
                http.delete_guild_command(guild_id, *id).exec().await?;
            }

            Ok(None)
        }
        // We never registered commands for this guild, and the new config doesn't
        // need them, so do nothing.
        (Some(_), None, None) => Ok(None),
        // Do nothing in this case.
        (None, None, _) => Ok(None),
    }
}

#[tracing::instrument(skip(state))]
pub(crate) async fn handle_command(state: crate::State, cmd: &ApplicationCommand) -> Result<()> {
    tracing::debug!(?cmd.data.id, ?state.cmd_states, "Executing command");
    if cmd.guild_id.is_none() {
        tracing::trace!("No guild ID for this command invocation");
        return Ok(());
    }

    let application_id = *state.application_id.read().await;
    if application_id.is_none() {
        tracing::trace!("No application ID yet");
        return Ok(());
    }

    let interaction_http = state.http.interaction(application_id.unwrap());

    let guild_id = cmd.guild_id.unwrap();

    let cmd_kind = {
        let cmd_states = state.cmd_states.read().await;
        let cmd_state = cmd_states.get(&guild_id).unwrap_or(&None);

        if let Some(cmd_state) = cmd_state {
            cmd_state.get_command_kind(cmd.data.id)
        } else {
            tracing::trace!(%guild_id, "No command state for guild");
            return Ok(());
        }
    };

    if let None = cmd_kind {
        tracing::trace!(?state.cmd_states, ?cmd.data.id, "Couldn't find command kind for command invocation");
        return Ok(());
    }

    tracing::trace!(?cmd_kind, "Determined command kind");

    match cmd_kind.unwrap() {
        CommandKind::Test => {
            if cmd.data.options.len() <= 0 {
                return Ok(());
            }

            if let CommandOptionValue::String(message) = &cmd.data.options[0].value {
                let guild_cfgs = state.guild_cfgs.read().await;

                if let Some(guild_config) = guild_cfgs.get(&guild_id) {
                    if let Some(message_filters) = &guild_config.messages {
                        let mut result = Ok(());
                        for filter in message_filters {
                            result = result.and(filter.filter_text(&message[..]));
                        }

                        let result_string = match result {
                            Ok(()) => "✅ Passed all filters".to_owned(),
                            Err(reason) => format!("❎ Failed filter: {}", reason),
                        };

                        interaction_http
                            .create_response(
                                cmd.id,
                                &cmd.token,
                                &InteractionResponse {
                                    kind: InteractionResponseType::ChannelMessageWithSource,
                                    data: Some(
                                        InteractionResponseDataBuilder::new()
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
                                                .build()])
                                            .build(),
                                    ),
                                },
                            )
                            .exec()
                            .await
                            .unwrap();
                    }
                }
            }
        }
        CommandKind::Arm => {
            state
                .armed
                .store(true, std::sync::atomic::Ordering::Relaxed);
            interaction_http
                .create_response(
                    cmd.id,
                    &cmd.token,
                    &InteractionResponse {
                        kind: InteractionResponseType::ChannelMessageWithSource,
                        data: Some(
                            InteractionResponseDataBuilder::new()
                                .flags(MessageFlags::EPHEMERAL)
                                .content("Chrysanthemum **armed**.".to_owned())
                                .build(),
                        ),
                    },
                )
                .exec()
                .await
                .unwrap();
        }
        CommandKind::Disarm => {
            state
                .armed
                .store(false, std::sync::atomic::Ordering::Relaxed);
            interaction_http
                .create_response(
                    cmd.id,
                    &cmd.token,
                    &InteractionResponse {
                        kind: InteractionResponseType::ChannelMessageWithSource,
                        data: Some(
                            InteractionResponseDataBuilder::new()
                                .flags(MessageFlags::EPHEMERAL)
                                .content("Chrysanthemum **disarmed**.".to_owned())
                                .build(),
                        ),
                    },
                )
                .exec()
                .await
                .unwrap();
        }
        CommandKind::Reload => {
            let result = crate::reload_guild_configs(&state).await;
            let embed = match result {
                Ok(()) => EmbedBuilder::new()
                    .title("Reload successful")
                    .color(0x32_a8_52)
                    .build(),
                Err((_, report)) => {
                    let report = report.to_string();
                    EmbedBuilder::new()
                        .title("Reload failure")
                        .field(
                            EmbedFieldBuilder::new("Reason", format!("```{}```", report)).build(),
                        )
                        .build()
                }
            };

            interaction_http
                .create_response(
                    cmd.id,
                    &cmd.token,
                    &InteractionResponse {
                        kind: InteractionResponseType::ChannelMessageWithSource,
                        data: Some(
                            InteractionResponseDataBuilder::new()
                                .flags(MessageFlags::EPHEMERAL)
                                .embeds(vec![embed])
                                .build(),
                        ),
                    },
                )
                .exec()
                .await
                .unwrap();
        }
    }

    Ok(())
}
