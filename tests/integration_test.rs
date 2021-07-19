use std::convert::TryInto;

use discordant::gateway::{self, Event, Gateway, Intents, connect_to_gateway};
use tokio::process::Command;
use tokio::time::{Duration, Instant, sleep};

use discordant::{http::{Client, CreateMessagePayload}, types::Snowflake};

async fn test_message_is_filtered(test_channel: Snowflake, test_log_channel: Snowflake, test_payload: CreateMessagePayload, client: &Client, gateway: &mut Gateway) {
    let expected_message = format!("TEST LOG\n{}", test_payload.content);
    let created_message = client.create_message(test_channel, test_payload).await.expect("couldn't send test message");
    let mut was_deleted = false;
    let mut was_logged = false;

    loop {
        tokio::select! {
            _ = sleep(Duration::from_secs(1)) => {
                panic!("timed out waiting for event");
            },
            event = gateway.next_event() => {
                match event {
                    Ok(Event::MessageDelete { id, .. }) if id == created_message.id => {
                        was_deleted = true;
                        if was_deleted && was_logged {
                            break;
                        }
                    },
                    Ok(Event::MessageCreate(message)) => {
                        if message.content == expected_message && message.channel_id == test_log_channel {
                            was_logged = true;
                            // Clean up.
                            client.delete_message(message.channel_id, message.id).await.expect("Could not delete log message");
                            if was_deleted && was_logged {
                                break;
                            }
                        }
                    }
                    _ => {},
                }
            }
        };
    }
}

#[tokio::test]
async fn test_chrysanthemum() {
    dotenv::dotenv().ok();

    let chrysanthemum_exe_location = env!("CARGO_BIN_EXE_chrysanthemum");
    let chrysanthemum_token =
        std::env::var("DISCORD_TOKEN").expect("Couldn't retrieve DISCORD_TOKEN variable");
    let observer_token = std::env::var("CHRYSANTHEMUM_TEST_OBSERVER_TOKEN").expect("Couldn't receive CHRYSANTHEMUM_TEST_OBSERVER_TOKEN variable");
    let test_channel = std::env::var("CHRYSANTHEMUM_TEST_CHANNEL").expect("Couldn't receive CHRYSANTHEMUM_TEST_CHANNEL variable").try_into().unwrap();
    let test_log_channel = std::env::var("CHRYSANTHEMUM_TEST_LOG_CHANNEL").expect("Couldn't receive CHRYSANTHEMUM_TEST_LOG_CHANNEL variable").try_into().unwrap();

    let mut chrysanthemum_command = Command::new(chrysanthemum_exe_location).arg("chrysanthemum.test.cfg.json").env("DISCORD_TOKEN", &chrysanthemum_token).spawn().expect("couldn't start chrysanthemum");

    let client = Client::new(&observer_token);
    let gateway_info = client.get_gateway_info().await.unwrap();
    let intents = Intents::GUILD_MESSAGES;
    let mut gateway = connect_to_gateway(&gateway_info.url, observer_token, intents).await.expect("Observer bot couldn't connect to gateway");

    // Wait a bit for Chrysanthemum to connect.
    sleep(Duration::from_secs(1)).await;

    test_message_is_filtered(test_channel, test_log_channel, CreateMessagePayload {
        content: format!("chrysanthemum_deny_word {}", std::time::UNIX_EPOCH.elapsed().unwrap().as_secs()),
    }, &client, &mut gateway).await;

    test_message_is_filtered(test_channel, test_log_channel, CreateMessagePayload {
        content: format!("chrysanthemum_deny_regex {}", std::time::UNIX_EPOCH.elapsed().unwrap().as_secs()),
    }, &client, &mut gateway).await;

    test_message_is_filtered(test_channel, test_log_channel, CreateMessagePayload {
        content: format!("discord.gg/notroblox {}", std::time::UNIX_EPOCH.elapsed().unwrap().as_secs()),
    }, &client, &mut gateway).await;

    test_message_is_filtered(test_channel, test_log_channel, CreateMessagePayload {
        content: format!("https://google.com/ {}", std::time::UNIX_EPOCH.elapsed().unwrap().as_secs()),
    }, &client, &mut gateway).await;

    chrysanthemum_command.kill().await.expect("couldn't kill chrysanthemum binary");
    gateway.close().await;
}
