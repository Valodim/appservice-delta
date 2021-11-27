use std::convert::TryFrom;

use anyhow::Result;
use deltachat::{chat::ChatId, config::Config};
use matrix_sdk::{
    room::{Joined, Room},
    ruma::{
        events::{
            reaction::{self, ReactionEventContent, SyncReactionEvent},
            room::{
                member::{MembershipState, SyncRoomMemberEvent},
                message::{
                    self, MessageType, RoomMessageEventContent, SyncRoomMessageEvent,
                    TextMessageEventContent,
                },
                topic::{RoomTopicEventContent, SyncRoomTopicEvent},
            },
            MessageEvent,
        },
        EventId, UserId,
    },
};

use crate::{
    cmdline, matrix_intents,
    types::{self, DeltaMessageFields},
    util, DeltaAppservice,
};

pub async fn handle_room_message(
    da: DeltaAppservice,
    room: Room,
    event: SyncRoomMessageEvent,
) -> anyhow::Result<()> {
    // ignore our own messages
    if event.sender == da.get_main_user().user_id().await.unwrap()
        || da.appservice.user_id_is_in_namespace(&event.sender)?
    {
        return Ok(());
    }
    let event_id = event.event_id;

    let msg_body = match &event.content.msgtype {
        MessageType::Text(TextMessageEventContent { body, .. }) => body.to_owned(),
        _ => return Ok(()),
    };

    if let Room::Joined(room) = room {
        let control_room_id = da.get_control_room_id().await;
        if let Some(message::Relation::Reply { in_reply_to }) = event.content.relates_to {
            handle_control_room_message_reply(da, room, event_id, msg_body, in_reply_to.event_id)
                .await?;
        } else if Some(room.room_id()) == control_room_id.as_ref() {
            handle_control_room_message(da, room, event_id, msg_body).await?;
        } else {
            handle_chat_room_message(da, room, event_id, msg_body).await?;
        }
    }
    Ok(())
}

pub async fn handle_room_member_event(
    da: DeltaAppservice,
    room: Room,
    ev: SyncRoomMemberEvent,
) -> anyhow::Result<()> {
    let user_id = UserId::try_from(ev.state_key.as_str())?;
    let is_main_user = user_id.localpart() == da.appservice.registration().sender_localpart;
    let is_appservice_user = da.appservice.user_id_is_in_namespace(user_id)?;

    if is_main_user && ev.content.membership == MembershipState::Invite {
        handle_room_main_user_invited(da, room, &ev.sender).await?;
    } else if !is_appservice_user && ev.content.membership == MembershipState::Leave {
        handle_room_user_leave(da, room).await?;
    }

    Ok(())
}

async fn handle_chat_room_message(
    da: DeltaAppservice,
    room: Joined,
    event_id: EventId,
    msg_body: String,
) -> Result<()> {
    if let Some(chat_id) = da.get_chat_id_by_room_id(room.room_id()).await {
        let msg_id = deltachat::chat::send_text_msg(&da.ctx, chat_id, msg_body)
            .await
            .unwrap();
        da.event_message_cache.insert(msg_id, event_id);
    }
    Ok(())
}

async fn handle_control_room_message(
    da: DeltaAppservice,
    control_room: Joined,
    event_id: EventId,
    msg_body: String,
) -> anyhow::Result<()> {
    let x = msg_body.splitn(2, ' ').collect::<Vec<_>>();
    let cmd = x.get(0).map(|s| *s).unwrap_or("");
    let args = x.get(1).map(|s| *s).unwrap_or("");

    if cmd == "configure" {
        handle_control_room_command_configure(da, control_room, event_id, args).await?;
        return Ok(());
    }

    if !da.ctx.is_configured().await? {
        let msg = RoomMessageEventContent::text_plain("Not configured.");
        control_room.send(msg, None).await?;
        return Ok(());
    }

    match cmd {
        "repl" => handle_control_room_command_repl(da, control_room, args).await?,
        "room" => handle_control_room_command_room(da, control_room, args).await?,
        "message" => handle_control_room_command_message(da, control_room, args).await?,
        _ => {
            let msg = RoomMessageEventContent::text_plain(format!(
                "Unknown command {}. Commands:\n - repl\n - configure",
                cmd
            ));
            control_room.send(msg, None).await?;
        }
    }
    return Ok(());
}

async fn handle_control_room_command_message(
    da: DeltaAppservice,
    control_room: Joined,
    arguments: &str,
) -> anyhow::Result<()> {
    let args = shell_words::split(arguments).expect("must parse");
    if args.len() < 1 || !deltachat::contact::may_be_valid_addr(&args[0]) {
        let msg =
            RoomMessageEventContent::text_plain("Usage: message <email> [full_name] [subject]");
        control_room.send(msg, None).await?;
        return Ok(());
    }

    let addr = &args[0];
    let name = args.get(1).map(String::as_str).unwrap_or("");
    let subject = args.get(2);

    let contact_id = deltachat::contact::Contact::create(&da.ctx, name, addr).await?;
    let chat_id = ChatId::create_for_contact(&da.ctx, contact_id).await?;
    let room_id = matrix_intents::create_room_for_chat(&da, control_room, chat_id).await?;

    if let Some(subject) = subject {
        let topic_event = RoomTopicEventContent::new(subject.to_owned());
        if let Some(room) = da.get_main_user().get_joined_room(&room_id) {
            room.send_state_event(topic_event, "").await?;
        }
    }

    Ok(())
}

async fn handle_control_room_command_room(
    da: DeltaAppservice,
    control_room: Joined,
    arguments: &str,
) -> Result<()> {
    if !da.ctx.is_configured().await.unwrap() {
        let msg = RoomMessageEventContent::text_plain("Not configured.");
        control_room.send(msg, None).await?;
        return Ok(());
    }

    match arguments.parse::<u32>() {
        Ok(chat_id) => {
            let chat_id = ChatId::new(chat_id);
            if let Some(room_id) = da.get_room_id_by_chat_id(chat_id).await {
                let msg = RoomMessageEventContent::text_plain(format!(
                    "Chat is already a room: {}",
                    room_id
                ));
                control_room.send(msg, None).await?;
                return Ok(());
            }

            matrix_intents::create_room_for_chat(&da, control_room, chat_id)
                .await
                .unwrap();
        }
        Err(_) => {
            let msg = RoomMessageEventContent::text_plain("room <chat_id>");
            control_room.send(msg, None).await?;
        }
    };

    Ok(())
}

async fn handle_control_room_command_configure(
    da: DeltaAppservice,
    room: Joined,
    event_id: EventId,
    arguments: &str,
) -> Result<()> {
    if let Err(err) = room.redact(&event_id, None, None).await {
        log::error!("{}", err);
    }

    if da.ctx.is_configured().await.unwrap() {
        let msg = RoomMessageEventContent::text_plain("Already configured.");
        room.send(msg, None).await?;
        return Ok(());
    }

    let args = shell_words::split(arguments).expect("must parse");
    if args.len() < 3 {
        let msg = RoomMessageEventContent::text_plain("Usage: configure <email> <password> <displayname> [login_user] [imap_host] [smtp_host]");
        room.send(msg, None).await?;
        return Ok(());
    }

    if !da.ctx.is_configured().await.unwrap() {
        for (k, v) in [
            (Config::Addr, Some(args[0].as_str())),
            (Config::MailPw, Some(args[1].as_str())),
            (Config::Displayname, Some(args[2].as_str())),
            (Config::MailUser, args.get(3).map(String::as_str)),
            (Config::MailServer, args.get(4).map(String::as_str)),
            (Config::SendServer, args.get(5).map(String::as_str)),
            (Config::E2eeEnabled, Some("0")),
            (Config::ShowEmails, Some("2")),
            (Config::BccSelf, Some("1")),
        ] {
            da.ctx.set_config(k, v).await.unwrap();
        }

        let msg = RoomMessageEventContent::text_plain("Configuring…");
        room.send(msg, None).await?;

        match da.ctx.configure().await {
            Err(err) => {
                let msg = RoomMessageEventContent::text_plain(format!("Error: {}", err));
                room.send(msg, None).await?;
            }
            Ok(()) => {
                let msg = RoomMessageEventContent::text_plain("Configured.");
                room.send(msg, None).await?;
                da.start().await;
            }
        }
    }

    Ok(())
}

async fn handle_control_room_command_repl(
    da: DeltaAppservice,
    room: Joined,
    repl_command: &str,
) -> Result<()> {
    let chat_id = da.get_repl_active_chat_id().await;

    let mut mut_chat_id = chat_id.clone();
    let repl_result = cmdline::cmdline(&da.ctx, repl_command, &mut mut_chat_id).await;

    if chat_id != mut_chat_id {
        da.set_repl_active_chat_id(mut_chat_id).await;
    }

    let msg = match repl_result {
        Ok(result) => RoomMessageEventContent::text_plain(result),
        Err(msg) => RoomMessageEventContent::text_plain(msg.to_string()),
    };
    room.send(msg, None).await.unwrap();

    Ok(())
}

async fn handle_control_room_message_reply(
    da: DeltaAppservice,
    room: Joined,
    event_id: EventId,
    msg_body: String,
    reply_event_id: EventId,
) -> Result<()> {
    let client = da.get_main_user();
    let delta_fields = match util::get_delta_event(&client, room.room_id(), &reply_event_id).await?
    {
        Some(MessageEvent {
            content:
                types::DeltaRoomEventContent {
                    delta_fields: Some(delta_fields),
                },
            ..
        }) => delta_fields,
        _ => {
            let msg = RoomMessageEventContent::text_plain(
                "Reply not sent: No chat_id in replied-to message",
            );
            room.send(msg, None).await?;
            return Ok(());
        }
    };

    // strip reply lines
    let msg_body = msg_body
        .lines()
        .filter(|line| !line.starts_with(">"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim_start()
        .to_owned();

    let chat_id = ChatId::new(delta_fields.chat_id);
    chat_id.accept(&da.ctx).await.unwrap();
    let msg_id = deltachat::chat::send_text_msg(&da.ctx, chat_id, msg_body)
        .await
        .unwrap();
    da.event_message_cache.insert(msg_id, event_id);

    Ok(())
}

pub async fn handle_room_topic_event(
    da: DeltaAppservice,
    room: Room,
    event: SyncRoomTopicEvent,
) -> Result<()> {
    if let Some(chat_id) = da.get_chat_id_by_room_id(room.room_id()).await {
        deltachat::chat::set_chat_name(&da.ctx, chat_id, &event.content.topic)
            .await
            .unwrap();
    }
    Ok(())
}

pub async fn handle_room_reaction_event(
    da: DeltaAppservice,
    room: Room,
    event: SyncReactionEvent,
) -> Result<()> {
    let main_user = da.get_main_user();
    if event.sender == main_user.user_id().await.unwrap()
        || da.appservice.user_id_is_in_namespace(&event.sender)?
    {
        log::debug!("skipping event: came from appservice user");
        return Ok(());
    }
    if da.get_control_room_id().await.as_ref() != Some(room.room_id()) {
        log::debug!("skipping event: not in control room");
        return Ok(());
    }
    let room = match room {
        Room::Joined(room) => room,
        _ => {
            log::warn!("skipping event: not in joined room");
            return Ok(());
        }
    };

    let SyncReactionEvent {
        content:
            ReactionEventContent {
                relates_to:
                    reaction::Relation {
                        event_id: react_event_id,
                        emoji,
                        ..
                    },
                ..
            },
        ..
    } = event;

    let delta_event =
        util::get_delta_event(&da.get_main_user(), room.room_id(), &react_event_id).await?;
    if let Some(MessageEvent {
        content:
            types::DeltaRoomEventContent {
                delta_fields: Some(DeltaMessageFields { chat_id, .. }),
            },
        sender: original_sender,
        ..
    }) = delta_event
    {
        let original_chat_id = ChatId::new(chat_id);
        match emoji.chars().next().unwrap() {
            '👍' => {
                return handle_control_room_reaction_thumbsup(
                    da,
                    room,
                    original_sender,
                    original_chat_id,
                )
                .await
            }
            '👎' => {
                return handle_control_room_reaction_thumbsdown(
                    da,
                    room,
                    original_sender,
                    original_chat_id,
                )
                .await
            }
            _ => log::warn!("Ignoring unknown reaction {}", emoji),
        }
    }
    Ok(())
}

async fn handle_control_room_reaction_thumbsup(
    da: DeltaAppservice,
    control_room: Joined,
    _original_sender: UserId,
    original_chat_id: ChatId,
) -> Result<()> {
    if da.get_room_id_by_chat_id(original_chat_id).await.is_some() {
        let msg = RoomMessageEventContent::text_plain("Chat is already a room.");
        control_room.send(msg, None).await?;
        return Ok(());
    }

    matrix_intents::create_room_for_chat(&da, control_room, original_chat_id).await?;
    Ok(())
}

async fn handle_room_user_leave(da: DeltaAppservice, room: Room) -> Result<()> {
    if let Some(chat_id) = da.get_chat_id_by_room_id(room.room_id()).await {
        da.clear_room_id_and_chat_id(room.room_id(), chat_id).await;
        matrix_intents::clear_room(&da, room).await.unwrap();
    }
    Ok(())
}

async fn handle_room_main_user_invited(
    da: DeltaAppservice,
    room: Room,
    sender: &UserId,
) -> anyhow::Result<()> {
    if let Some(room) = da.get_control_room().await {
        let msg = RoomMessageEventContent::text_plain("Not joining other invite!");
        room.send(msg, None).await?;
        return Ok(());
    }

    let client = da.get_main_user();
    client.join_room_by_id(room.room_id()).await?;
    da.set_control_room_id(room.room_id()).await?;
    da.set_owner_user_id(sender).await?;

    Ok(())
}

async fn handle_control_room_reaction_thumbsdown(
    da: DeltaAppservice,
    _room: Joined,
    _original_sender: UserId,
    original_chat_id: ChatId,
) -> Result<()> {
    original_chat_id.block(&da.ctx).await.unwrap();
    Ok(())
}
