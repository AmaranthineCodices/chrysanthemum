use std::{collections::HashMap, sync::Arc, time::Duration};

use reqwest::{Url, header::{HeaderMap, HeaderName, HeaderValue}};
use tokio::{sync, time::Instant};

use discordant::{http::CreateMessagePayload, types::Snowflake};

struct DeleteMessage {
    channel: Snowflake,
    message: Snowflake,
}

struct DeleteReactions {
    channel: Snowflake,
    message: Snowflake,
    emoji: discordant::types::ReactionEmoji,
}

struct SendMessage {
    channel: Snowflake,
    message: String,
    batch_allowed: bool,
}

struct SendWebRequest {
    url: Url,
    method: String,
    headers: HashMap<String, String>,
    payload: String,
}

struct ActionDispatcher {
    delete_message_sender: sync::mpsc::UnboundedSender<DeleteMessage>,
    delete_reactions_sender: sync::mpsc::UnboundedSender<DeleteReactions>,
    send_message_sender: sync::mpsc::UnboundedSender<SendMessage>,
    send_web_request_sender: sync::mpsc::UnboundedSender<SendWebRequest>,
    shutdown_sender: sync::broadcast::Sender<()>,
}

type ActionChannelPair<T> = (
    sync::mpsc::UnboundedSender<T>,
    sync::mpsc::UnboundedReceiver<T>,
);

impl ActionDispatcher {
    fn new(token: &str) -> ActionDispatcher {
        let (delete_message_sender, mut delete_message_receiver): ActionChannelPair<DeleteMessage> =
            sync::mpsc::unbounded_channel();
        let (delete_reactions_sender, mut delete_reactions_receiver): ActionChannelPair<
            DeleteReactions,
        > = sync::mpsc::unbounded_channel();
        let (send_message_sender, mut send_message_receiver): ActionChannelPair<SendMessage> =
            sync::mpsc::unbounded_channel();
        let (send_web_request_sender, mut send_web_request_receiver): ActionChannelPair<
            SendWebRequest,
        > = sync::mpsc::unbounded_channel();
        let (shutdown_sender, shutdown_receiver) = sync::broadcast::channel(1);

        let discordant_client = Arc::new(discordant::http::Client::new(token));

        {
            let discordant_client = discordant_client.clone();
            tokio::spawn(async move {
                loop {
                    let delete_message = delete_message_receiver.recv().await;

                    match delete_message {
                        Some(delete_message) => {
                            let result = discordant_client
                                .delete_message(delete_message.channel, delete_message.message)
                                .await;

                            if let Err(err) = result {
                                log::error!(
                                    "unable to delete message {}: {:?}",
                                    delete_message.message,
                                    err
                                );
                            }
                        }
                        None => break,
                    }
                }
            });
        }

        {
            let discordant_client = discordant_client.clone();
            tokio::spawn(async move {
                loop {
                    let delete_reactions = delete_reactions_receiver.recv().await;

                    match delete_reactions {
                        Some(delete_reactions) => {
                            let result = discordant_client
                                .delete_reactions_for_emoji(
                                    delete_reactions.channel,
                                    delete_reactions.message,
                                    &delete_reactions.emoji,
                                )
                                .await;

                            if let Err(err) = result {
                                log::error!(
                                    "unable to delete reaction emoji {:?} for message {}: {:?}",
                                    delete_reactions.emoji,
                                    delete_reactions.message,
                                    err
                                );
                            }
                        }
                        None => break,
                    }
                }
            });
        }

        {
            let discordant_client = discordant_client.clone();

            const MAX_MESSAGE_LEN: usize = 2_000;

            tokio::spawn(async move {
                let delay_duration = Duration::from_secs(1);

                let mut last_send = Instant::now();

                // channel_id -> pending messages
                let mut queued_batch_messages: HashMap<Snowflake, Vec<String>> = HashMap::new();

                fn compile_composite_messages(
                    queue: &mut HashMap<Snowflake, Vec<String>>,
                ) -> HashMap<Snowflake, Vec<String>> {
                    let mut result = HashMap::new();
                    for (channel, messages) in queue.iter_mut() {
                        result.insert(*channel, Vec::new());
                        let mut current_blob = String::new();
                        for message in messages.drain(..) {
                            // It's not safe to blindly truncate the message; we
                            // could end up cleaving through a markdown formatting
                            // symbol or something. The only thing we can do at
                            // this point is error.
                            if message.len() > MAX_MESSAGE_LEN {
                                log::error!("message is too long to send");
                                continue;
                            }

                            // add 2 to account for the two newline characters.
                            if current_blob.len() + message.len() + 2 >= MAX_MESSAGE_LEN {
                                let finished_blob = current_blob;
                                result.get_mut(&channel).unwrap().push(finished_blob);
                                current_blob = String::new();
                            }

                            current_blob.push('\n');
                            current_blob.push('\n');
                            current_blob.push_str(&message);
                        }

                        result.get_mut(&channel).unwrap().push(current_blob);
                    }

                    result
                }

                loop {
                    tokio::select! {
                        _ = tokio::time::sleep_until(last_send + delay_duration) => {
                            last_send = Instant::now();
                            let composite_messages = compile_composite_messages(&mut queued_batch_messages);

                            for (channel, messages) in composite_messages {
                                for message in messages {
                                    let result = discordant_client.create_message(channel, CreateMessagePayload {
                                        content: message,
                                    }).await;

                                    if let Err(err) = result {
                                        log::error!("unable to send message to channel {}: {:?}", channel, err);
                                    }
                                }
                            }
                        },
                        message = send_message_receiver.recv() => {
                            match message {
                                Some(message) => {
                                    if !message.batch_allowed {
                                        let result = discordant_client.create_message(message.channel, CreateMessagePayload {
                                            content: message.message,
                                        }).await;

                                        if let Err(err) = result {
                                            log::error!("unable to send message to channel {}: {:?}", message.channel, err);
                                        }
                                    }
                                    else {
                                        if !queued_batch_messages.contains_key(&message.channel) {
                                            queued_batch_messages.insert(message.channel, Vec::new());
                                        }

                                        queued_batch_messages.get_mut(&message.channel).unwrap().push(message.message);
                                    }
                                },
                                None => break,
                            }
                        }
                    }
                }
            });
        }

        {
            tokio::spawn(async move {
                let client = reqwest::Client::builder()
                    .user_agent(concat!(
                        env!("CARGO_PKG_NAME"),
                        "/",
                        env!("CARGO_PKG_VERSION")
                    ))
                    .build()
                    .unwrap();

                loop {
                    let request = send_web_request_receiver.recv().await;
                    match request {
                        Some(request) => {
                            let method = reqwest::Method::from_bytes(request.method.as_bytes());
                            if let Ok(method) = method {
                                let mut header_map = HeaderMap::new();
                                for (header, value) in &request.headers {
                                    let header_name = HeaderName::from_bytes(header.as_bytes());
                                    let header_value = HeaderValue::from_str(&value);
                                    match (header_name, header_value) {
                                        (Ok(name), Ok(value)) => {
                                            header_map.insert(name, value);
                                        }
                                        (Err(err), _) => {
                                            log::error!("unable to convert header name {} into reqwest HeaderName type: {:?}", header, err);
                                        },
                                        (_, Err(err)) => {
                                            log::error!("unable to convert header value {} into reqwest HeaderValue type: {:?}", value, err);
                                        },
                                    }
                                }

                                let response =
                                    client.request(method, request.url).headers(header_map).body(request.payload).send().await;
                                
                                if let Err(err) = response {
                                    log::error!("unable to send web request: {:?}", err);
                                }
                            } else {
                                log::error!(
                                    "unable to convert method string {} to reqwest Method type",
                                    request.method
                                );
                            }
                        }
                        None => break,
                    }
                }
            });
        }

        ActionDispatcher {
            delete_message_sender,
            delete_reactions_sender,
            send_message_sender,
            send_web_request_sender,
            shutdown_sender,
        }
    }

    pub fn delete_message(&self, channel: Snowflake, message: Snowflake) {
        self.delete_message_sender.send(DeleteMessage {
            channel,
            message,
        });
    }

    pub fn delete_reactions(&self, channel: Snowflake, message: Snowflake, emoji: discordant::types::ReactionEmoji) {
        self.delete_reactions_sender.send(DeleteReactions {
            channel,
            message,
            emoji,
        });
    }

    pub fn send_message(&self, channel: Snowflake, message: String, batch_allowed: bool) {
        self.send_message_sender.send(SendMessage {
            channel,
            message,
            batch_allowed,
        });
    }

    pub fn send_web_request(&self, url: Url, method: String, headers: HashMap<String, String>, payload: String) {
        self.send_web_request_sender.send(SendWebRequest {
            url,
            method,
            headers,
            payload,
        });
    }
}
