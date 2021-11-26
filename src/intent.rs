use deltachat::constants::Viewtype;
use deltachat::mimeparser::SystemMessage;
use matrix_sdk::{
    ruma::{
        api::client::r0::message::send_message_event,
        events::{room::message::RoomMessageEventContent, MessageEventContent},
        serde::Raw,
        RoomId,
    },
    uuid::Uuid,
    Client, Result,
};

use crate::types;

pub async fn send_delta_message(
    client: &Client,
    room_id: &RoomId,
    message: deltachat::message::Message,
) -> Result<()> {
    if message.is_system_message() {
        send_delta_system_message(client, room_id, message).await
    } else {
        send_delta_plain_message(client, room_id, message).await
    }
}

async fn send_delta_system_message(
    _client: &Client,
    _room_id: &RoomId,
    message: deltachat::message::Message,
) -> Result<()> {
    log::info!("skipping system message: {:?}", message);
    Ok(())
}

async fn send_delta_plain_message(
    client: &Client,
    room_id: &RoomId,
    message: deltachat::message::Message,
) -> Result<()> {
    let msg = match message.get_viewtype() {
        /*
        Viewtype::Image | Viewtype::Gif => {
            let mime = message.get_filemime().unwrap();
            let path = message.get_filename().unwrap();
            let f = File::open(path).unwrap();
            let mut reader = BufReader::new(f);
            let response = client.upload(&mime.parse().unwrap(), &mut reader).await?;
            let msgtype = ImageMessageEventContent::plain(
                message.get_text().unwrap_or_default().to_owned(),
                response.content_uri,
                None,
            );
            RoomMessageEventContent::new(MessageType::Image(msgtype))
        }
        */
        Viewtype::Text => {
            RoomMessageEventContent::text_plain(message.get_text().unwrap_or_default())
        }
        other => RoomMessageEventContent::notice_plain(format!("(unhandled view type: {}", other)),
    };
    let content = types::DeltaRoomMessageEventContent {
        delta_fields: Some(types::DeltaMessageFields {
            external_url: Some(format!("mid:{}", message.get_rfc724_mid())),
            msg_id: message.get_id().to_u32(),
            chat_id: message.get_chat_id().to_u32(),
        }),
        msg,
    };

    send_message(client, room_id, content).await
}

// TODO replace with room.send() once room sync is available
pub async fn send_message(
    client: &Client,
    room_id: &RoomId,
    content: impl MessageEventContent,
) -> Result<()> {
    let event_type = content.event_type().to_owned();
    let content = serde_json::to_value(content)?;
    let txn_id = Uuid::new_v4().to_string();
    let content = serde_json::value::to_raw_value(&content)?;

    let request = send_message_event::Request::new_raw(
        room_id,
        &txn_id,
        &event_type,
        Raw::from_json(content),
    );

    let response = client.send(request, None).await?;
    log::debug!("send_message response: {:?}", response);

    Ok(())
}
