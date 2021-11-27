use crate::matrix_intents;

use deltachat::chat::ChatId;
use deltachat::message::{Message, MsgId};
use deltachat::EventType;
use matrix_sdk::ruma::api::client::r0::read_marker::set_read_marker;
use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;

use crate::DeltaAppservice;

pub async fn callback_delta_event(da: &DeltaAppservice, event: EventType) {
    match event {
        EventType::IncomingMsg { chat_id, msg_id } => {
            log::info!("{:?}", event);
            handle_incoming_message(da, chat_id, msg_id)
                .await
                .unwrap();
        }
        EventType::MsgDelivered { chat_id, msg_id } => {
            log::info!("{:?}", event);
            handle_delivered_message(da, chat_id, msg_id)
                .await
                .unwrap();
        }
        EventType::MsgFailed { chat_id, msg_id } => {
            log::info!("{:?}", event);
            handle_failed_message(da, chat_id, msg_id)
                .await
                .unwrap();
        }
        EventType::MsgRead {
            chat_id,
            msg_id,
            contact_id,
        } => {
            log::info!("{:?}", event);
            handle_read_message(da, chat_id, msg_id, contact_id)
                .await
                .unwrap();
        }
        EventType::ContactsChanged(Some(contact_id)) => {
            handle_contact_changed(da, contact_id).await.unwrap();
        }
        EventType::Info(msg) => {
            log::info!("{}", msg);
        }
        EventType::Warning(msg) => {
            log::warn!("{}", msg);
        }
        EventType::Error(msg) => {
            log::error!("{}", msg);
        }
        event => {
            log::info!("{:?}", event);
        }
    }
}

async fn handle_contact_changed(da: &DeltaAppservice, contact_id: u32) -> anyhow::Result<()> {
    da.get_email_user_by_id(contact_id, true).await?;
    Ok(())
}

async fn handle_delivered_message(
    da: &DeltaAppservice,
    chat_id: ChatId,
    msg_id: MsgId,
) -> anyhow::Result<()> {
    if let Some(room_id) = da.get_room_id_by_chat_id(chat_id).await {
        if let Some(room) = da.get_main_user().get_joined_room(&room_id) {
            if let Some(event_id) = da.event_message_cache.get(&msg_id) {
                room.read_marker(&event_id, Some(&event_id)).await?
            }
        }
    }
    Ok(())
}

async fn handle_read_message(
    da: &DeltaAppservice,
    chat_id: ChatId,
    msg_id: MsgId,
    contact_id: u32,
) -> anyhow::Result<()> {
    if let Some(room_id) = da.get_room_id_by_chat_id(chat_id).await {
        if let Ok((_, user_client)) = da.get_email_user_by_id(contact_id, false).await {
            if let Some(event_id) = da.event_message_cache.get(&msg_id) {
                let mut request = set_read_marker::Request::new(&room_id, &event_id);
                request.read_receipt = Some(&event_id);
                user_client.send(request, None).await?;
            }
        }
    }
    Ok(())
}

async fn handle_incoming_message(
    da: &DeltaAppservice,
    chat_id: ChatId,
    msg_id: MsgId,
) -> anyhow::Result<()> {
    if let Some(control_room_id) = da.get_control_room_id().await {
        let message = Message::load_from_db(&da.ctx, msg_id).await.unwrap();
        // let text = message::get_msg_info(&delta.ctx, msg_id).await.unwrap();
        let (_, user_client) = da
            .get_email_user_by_id(message.get_from_id(), false)
            .await?;

        let room_id = if let Some(room_id) = da.get_room_id_by_chat_id(chat_id).await {
            // mark messages as noticed if we have a specific room
            deltachat::chat::marknoticed_chat(&da.ctx, chat_id)
                .await
                .unwrap();
            room_id
        } else {
            control_room_id
        };

        matrix_intents::send_delta_message(&da, &user_client, &room_id, message).await?
    } else {
        log::error!("no control room registered");
    }
    Ok(())
}

async fn handle_failed_message(
    da: &DeltaAppservice,
    chat_id: ChatId,
    msg_id: MsgId,
) -> anyhow::Result<()> {
    let msg = RoomMessageEventContent::text_plain("Sending message failed");

    let message = Message::load_from_db(&da.ctx, msg_id).await.unwrap();
    let (_, user_client) = da
        .get_email_user_by_id(message.get_from_id(), false)
        .await?;

    if let Some(chat_room_id) = da.get_room_id_by_chat_id(chat_id).await {
        if let Some(room) = user_client.get_joined_room(&chat_room_id) {
            room.send(msg, None).await?;
            return Ok(());
        }
    }

    if let Some(control_room_id) = da.get_control_room_id().await {
        if let Some(room) = user_client.get_joined_room(&control_room_id) {
            room.send(msg, None).await?;
        }
    }

    Ok(())
}
