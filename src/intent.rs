use std::{convert::TryInto, io::BufReader};

use deltachat::{constants::Viewtype, dc_tools::dc_open_file_std};
use matrix_sdk::{Client, Result, ruma::{EventId, MxcUri, RoomId, api::client::r0::message::send_message_event, events::{MessageEventContent, room::{ImageInfo, message::{FileInfo, FileMessageEventContent, ImageMessageEventContent, MessageType, RoomMessageEventContent}}}, serde::Raw}, uuid::Uuid};

use crate::{types, DeltaAppservice};

pub async fn send_delta_message(
    da: &DeltaAppservice,
    client: &Client,
    room_id: &RoomId,
    message: deltachat::message::Message,
) -> anyhow::Result<()> {
    if message.is_system_message() {
        send_delta_system_message(client, room_id, message).await
    } else {
        send_delta_plain_message(da, client, room_id, message).await
    }
}

async fn send_delta_system_message(
    _client: &Client,
    _room_id: &RoomId,
    message: deltachat::message::Message,
) -> anyhow::Result<()> {
    log::info!("skipping system message: {:?}", message);
    Ok(())
}

async fn send_delta_plain_message(
    da: &DeltaAppservice,
    client: &Client,
    room_id: &RoomId,
    message: deltachat::message::Message,
) -> anyhow::Result<()> {
    match message.get_viewtype() {
        Viewtype::File => {
            if let Some(file_uri) = upload_delta_file(da, client, &message).await? {
                let file_info = get_file_info(da, &message).await;
                let content = RoomMessageEventContent::new(MessageType::File(
                    FileMessageEventContent::plain(
                        message.get_filename().unwrap_or_default(),
                        file_uri,
                        file_info,
                    ),
                ));
                send_delta_plain_message_content(da, client, room_id, &message, content).await?;
            }
        }
        Viewtype::Image | Viewtype::Gif => {
            if let Some(file_uri) = upload_delta_file(da, client, &message).await? {
                let image_info = get_image_info(da, &message).await;
                let content = RoomMessageEventContent::new(MessageType::Image(
                    ImageMessageEventContent::plain(
                        message.get_filename().unwrap_or_default(),
                        file_uri,
                        image_info,
                    ),
                ));
                send_delta_plain_message_content(da, client, room_id, &message, content).await?;
            }
        }
        other => {
            RoomMessageEventContent::notice_plain(format!("(unhandled view type: {})", other));
        }
    };

    if let Some(text) = message.get_text() {
        let content = RoomMessageEventContent::text_plain(text);
        send_delta_plain_message_content(da, client, room_id, &message, content).await?;
    }

    Ok(())
}

async fn upload_delta_file(
    da: &DeltaAppservice,
    client: &Client,
    message: &deltachat::message::Message,
) -> anyhow::Result<Option<MxcUri>> {
    if let Some(mime) = message.get_filemime() {
        if let Some(filename) = message.get_file(&da.ctx) {
            let f = dc_open_file_std(&da.ctx, filename)?;
            let mut reader = BufReader::new(f);
            let response = client.upload(&mime.parse().unwrap(), &mut reader).await?;
            return Ok(Some(response.content_uri));
        }
    }
    Ok(None)
}

async fn get_file_info(
    da: &DeltaAppservice,
    message: &deltachat::message::Message,
) -> Option<Box<FileInfo>> {
    if let Some(mime) = message.get_filemime() {
        let mut file_info = FileInfo::new();
        file_info.mimetype = Some(mime);
        file_info.size = Some(message.get_filebytes(&da.ctx).await.try_into().unwrap());
        Some(Box::new(file_info))
    } else {
        None
    }
}

async fn get_image_info(
    da: &DeltaAppservice,
    message: &deltachat::message::Message,
) -> Option<Box<ImageInfo>> {
    if let Some(mime) = message.get_filemime() {
        let mut image_info = ImageInfo::new();
        image_info.mimetype = Some(mime);
        image_info.width = Some(message.get_width().try_into().unwrap());
        image_info.height = Some(message.get_height().try_into().unwrap());
        image_info.size = Some(message.get_filebytes(&da.ctx).await.try_into().unwrap());
        Some(Box::new(image_info))
    } else {
        None
    }
}

async fn send_delta_plain_message_content(
    da: &DeltaAppservice,
    client: &Client,
    room_id: &RoomId,
    message: &deltachat::message::Message,
    msg: RoomMessageEventContent,
) -> Result<()> {
    let content = types::DeltaRoomMessageEventContent {
        delta_fields: Some(types::DeltaMessageFields {
            external_url: Some(format!("mid:{}", message.get_rfc724_mid())),
            msg_id: message.get_id().to_u32(),
            chat_id: message.get_chat_id().to_u32(),
        }),
        msg,
    };

    let event_id = if let Some(room) = client.get_joined_room(room_id) {
        room.send(content, None).await?.event_id
    } else {
        log::warn!("User {} not joined in room {}, falling back to send_message", client.user_id().await.unwrap(), room_id);
        send_message(client, room_id, content).await?
    };

    da.event_message_cache.insert(message.get_id().clone(), event_id);

    Ok(())
}

// TODO replace with room.send() once room sync is available
async fn send_message(
    client: &Client,
    room_id: &RoomId,
    content: impl MessageEventContent,
) -> Result<EventId> {
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
    Ok(response.event_id)
}
