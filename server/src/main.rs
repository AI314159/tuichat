// The primary server for the chat program.
// Some notes on implementation:
//  - Users are often stored as their usernames. This is because strings are easy.
//    However, this is the lazy way of doing it. It should be instances of "User",
//    or as a string of a user ID, which I do not currently have a system for.

// KNOWN BUGS (FIXME):
// - When a user changes their name, the server is aware, but the client is not.
//   This means that the client is sending messages with the old username, but
//   the server expects the new username, and thus, the message is never sent,
//   as the server thinks that impersonation is happening.

use futures::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};
use common::{decode_message, encode_message, encode_user_response, decode_user, Message, User, UsernameRequestResponse};
use std::env;

mod channel_utils;
mod names_resolver;

const HELP_MESSAGE: &str = include_str!("help_msg.txt");

macro_rules! handle {
    ($result:expr) => {
        match $result {
            Ok(ok) => ok,
            Err(err) => break Err(err.into()),
        }
    };
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        println!("Usage: {} <SERVER IP>:<PORT>", args[0]);
        return Ok(());
    }

    let server = TcpListener::bind(&args[1]).await?;
    let names = names_resolver::Names::new();
    let channels = channel_utils::Channels::new();
    names.insert("Server".to_owned());
    loop {
        let (tcp, _) = server.accept().await?;
        tokio::spawn(handle_user(tcp, names.clone(), channels.clone()));
    }
}

fn server_message(s: &str) -> Message {
    Message {
        user: User {
            username: "Server".to_owned(),
        },
        contents: s.to_owned(),
        timestamp: 0,
    }
}

fn encoded_server_message(s: &str) -> String {
    encode_message(&server_message(s))
}

async fn handle_user(
    mut tcp: TcpStream,
    names: names_resolver::Names,
    channels: channel_utils::Channels,
) -> anyhow::Result<()> {
    let (reader, writer) = tcp.split();
    let mut stream = FramedRead::new(reader, LinesCodec::new());
    let mut sink = FramedWrite::new(writer, LinesCodec::new());

    // Wait for the username. If the user is using our client, then it should be 
    // submitted, no prompt necessary because the client takes care of that.
    let mut name = loop {
        if let Some(Ok(user_msg)) = stream.next().await {
            let user = decode_user(&user_msg)?;
            let new_name = user.username.trim().to_owned();
            if names.insert(new_name.clone()) {
                break new_name;
            } else {
                sink.send(encode_user_response(
                    &UsernameRequestResponse { status: 1 },
                )).await?;
            }
        }
    };
    // 0 is an OK status
    sink.send(encode_user_response( &UsernameRequestResponse { status: 0 })).await?;

    sink.send(encoded_server_message(&format!("{HELP_MESSAGE}\nYou are {name}."))).await?;

    let mut channel_name = channel_utils::DEFAULT_CHANNEL_NAME.to_owned();
    let mut tx = channels.join(&channel_name);
    let mut rx = tx.subscribe();

    tx.send(encoded_server_message(&format!("{name} just joined #{channel_name}.")))?;

    let result: anyhow::Result<()> = loop {
        tokio::select! {
            user_msg = stream.next() => {
                let user_msg = match user_msg {
                    Some(msg) => handle!(msg),
                    None => break Ok(()),
                };
                let decoded_message = handle!(decode_message(&user_msg));
                let message_contents = decoded_message.contents.clone();

                if message_contents.starts_with("/help") {
                    handle!(sink.send(encoded_server_message(HELP_MESSAGE)).await);
                } else if message_contents.starts_with("/setname") {
                    let new_name = match message_contents
                        .split_ascii_whitespace()
                        .nth(1) {
                            Some(name) => name,
                            None => {
                                handle!(sink.send(encoded_server_message(&format!("Usage: /setname <new_name>"))).await);
                                continue;
                            }
                        }
                        .to_owned();
                    let changed_name = names.insert(new_name.clone());
                    if changed_name {
                        handle!(tx.send(encoded_server_message(&format!("@{name} is now @{new_name}"))));
                        names.remove_name(&name);
                        name = new_name;
                    } else {
                        handle!(sink.send(encoded_server_message(&format!("@{new_name} is already taken."))).await);
                    }
                } else if message_contents.starts_with("/exit") {
                    break Ok(());
                } else if message_contents.starts_with("/jc") {
                    let new_channel = match message_contents
                                .split_ascii_whitespace()
                                .nth(1) {
                                    Some(channel) => channel,
                                    None => {
                                        handle!(sink.send(encoded_server_message(&format!("Usage: /jc <channel_name>"))).await);
                                        continue;
                                    }
                                }
                                .to_owned();

                            if new_channel == channel_name {
                                handle!(sink.send(encoded_server_message(&format!("You are in #{channel_name} already."))).await);
                                continue;
                            }

                            handle!(tx.send(encoded_server_message(&format!("{name} left #{channel_name}"))));
                            tx = channels.join(&new_channel);
                            rx = tx.subscribe();
                            channel_name = new_channel;

                            handle!(tx.send(encoded_server_message(&format!("{name} joined #{channel_name}"))));
                } else {
                    // FIXME: we should check if the username is what we think it is.
                    // For now, though, we just pretend that it is what we think it is
                    let mut message_to_send = decoded_message;
                    message_to_send.user.username = name.clone();
                        handle!(tx.send(
                            encode_message(&message_to_send)
                        ));
                }
            },
            remote_message = rx.recv() => {
                let remote_message = handle!(remote_message);
                handle!(sink.send(remote_message).await);
            },
        }
    };

    tx.send(encoded_server_message(&format!("{name} disconnected.")))?;

    names.remove_name(&name);
    result
}
