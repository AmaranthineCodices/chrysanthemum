use color_eyre::eyre::Result;
use twilight_http::client::InteractionClient;
use twilight_model::application::command::CommandType;
use twilight_model::application::interaction::InteractionData;
use twilight_model::{
    application::{
        command::{CommandOption, CommandOptionType},
        interaction::{
            application_command::{CommandData, CommandOptionValue},
            Interaction,
        },
    },
    channel::{message::MessageFlags, ChannelType},
    guild::Permissions,
    http::interaction::{InteractionResponse, InteractionResponseType},
    id::{marker::GuildMarker, Id},
};
use twilight_util::builder::command::CommandBuilder;
use twilight_util::builder::{
    embed::{EmbedBuilder, EmbedFieldBuilder},
    InteractionResponseDataBuilder,
};

use crate::config::SlashCommands;

const TEST_COMMAND: &str = "chrysanthemum-test";
const ARM_COMMAND: &str = "chrysanthemum-arm";
const DISARM_COMMAND: &str = "chrysanthemum-disarm";
const RELOAD_COMMAND: &str = "chrysanthemum-reload";

#[tracing::instrument(skip(http))]
pub(crate) async fn create_commands_for_guild(
    http: &InteractionClient<'_>,
    guild_id: Id<GuildMarker>,
) -> Result<()> {
    http.set_guild_commands(
        guild_id,
        &vec![
            CommandBuilder::new(
                TEST_COMMAND,
                "Test a message against Chrysanthemum's filter.",
                CommandType::ChatInput,
            )
            .default_member_permissions(Permissions::MANAGE_MESSAGES)
            .option(CommandOption {
                name: "message".to_owned(),
                description: "The message to test.".to_owned(),
                channel_types: Some(vec![
                    ChannelType::GuildText,
                    ChannelType::GuildVoice,
                    ChannelType::GuildCategory,
                    ChannelType::GuildAnnouncement,
                ]),
                kind: CommandOptionType::String,
                max_length: Some(2000),
                min_length: Some(1),
                autocomplete: None,
                choices: None,
                description_localizations: None,
                max_value: None,
                min_value: None,
                name_localizations: None,
                options: None,
                required: Some(true),
            })
            .build(),
            CommandBuilder::new(ARM_COMMAND, "Arms Chrysanthemum.", CommandType::ChatInput)
                .default_member_permissions(Permissions::ADMINISTRATOR)
                .build(),
            CommandBuilder::new(
                DISARM_COMMAND,
                "Disarms Chrysanthemum.",
                CommandType::ChatInput,
            )
            .default_member_permissions(Permissions::ADMINISTRATOR)
            .build(),
            CommandBuilder::new(
                RELOAD_COMMAND,
                "Reloads Chrysanthemum configurations from disk.",
                CommandType::ChatInput,
            )
            .default_member_permissions(Permissions::ADMINISTRATOR)
            .build(),
        ],
    )
    .await?;

    Ok(())
}

#[tracing::instrument(skip(http, new_config))]
pub(crate) async fn update_guild_commands(
    http: &InteractionClient<'_>,
    guild_id: Id<GuildMarker>,
    new_config: Option<&SlashCommands>,
) -> Result<()> {
    match new_config {
        // Command isn't registered.
        Some(_) => {
            create_commands_for_guild(http, guild_id).await?;
            Ok(())
        }
        // Need to delete the commands.
        None => {
            http.set_guild_commands(guild_id, &[]).await?;
            Ok(())
        }
    }
}

#[tracing::instrument(skip(state))]
pub(crate) async fn handle_command(
    state: crate::State,
    interaction: &Interaction,
    cmd: &CommandData,
) -> Result<()> {
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
    let cmd_data = interaction
        .data
        .as_ref()
        .map(|d| match d {
            InteractionData::ApplicationCommand(d) => Some(d.as_ref()),
            _ => None,
        })
        .unwrap_or(None);

    match cmd_data {
        Some(cmd_data) => match cmd_data.name.as_str() {
            TEST_COMMAND => {
                if cmd.options.is_empty() {
                    return Ok(());
                }

                if let CommandOptionValue::String(message) = &cmd.options[0].value {
                    let guild_cfgs = state.guild_cfgs.read().await;

                    if let Some(guild_config) = guild_cfgs.get(&guild_id) {
                        if let Some(message_filters) = &guild_config.messages {
                            let result = message_filters
                                .iter()
                                .map(|f| f.filter_text(&message[..]).map_err(|e| (f, e)))
                                .find(Result::is_err)
                                .map(|r| r.unwrap_err());

                            let mut builder = EmbedBuilder::new().title("Test filter").field(
                                EmbedFieldBuilder::new("Input", format!("```{}```", message))
                                    .build(),
                            );

                            match result {
                                Some((filter, reason)) => {
                                    builder = builder
                                        .field(EmbedFieldBuilder::new(
                                            "Status",
                                            format!("❌ Failed: {}", reason),
                                        ))
                                        .field(EmbedFieldBuilder::new("Filter", &filter.name));
                                }
                                None => {
                                    builder = builder.field(EmbedFieldBuilder::new(
                                        "Status",
                                        "✅ Passed all filters",
                                    ));
                                }
                            }

                            interaction_http
                                .create_response(
                                    interaction.id,
                                    &interaction.token,
                                    &InteractionResponse {
                                        kind: InteractionResponseType::ChannelMessageWithSource,
                                        data: Some(
                                            InteractionResponseDataBuilder::new()
                                                .flags(MessageFlags::EPHEMERAL)
                                                .embeds(vec![builder.build()])
                                                .build(),
                                        ),
                                    },
                                )
                                .await
                                .unwrap();
                        }
                    }
                }
            }
            ARM_COMMAND => {
                state
                    .armed
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                interaction_http
                    .create_response(
                        interaction.id,
                        &interaction.token,
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
                    .await
                    .unwrap();
            }
            DISARM_COMMAND => {
                state
                    .armed
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                interaction_http
                    .create_response(
                        interaction.id,
                        &interaction.token,
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
                    .await
                    .unwrap();
            }
            RELOAD_COMMAND => {
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
                                EmbedFieldBuilder::new("Reason", format!("```{}```", report))
                                    .build(),
                            )
                            .build()
                    }
                };

                interaction_http
                    .create_response(
                        interaction.id,
                        &interaction.token,
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
                    .await
                    .unwrap();
            }
            _ => {
                tracing::trace!("Received unhandleable interaction: unknown command name.");
            }
        },
        None => {
            tracing::trace!("Received unhandleable interaction: not an application command.");
        }
    }

    Ok(())
}
