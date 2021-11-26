use std::{convert::TryInto, io::BufReader};

use deltachat::{constants::Viewtype, dc_tools::dc_open_file_std};
use matrix_sdk::{Client, Result, ruma::{MxcUri, RoomId, api::client::r0::message::send_message_event, events::{MessageEventContent, room::{ImageInfo, message::{ImageMessageEventContent, MessageType, RoomMessageEventContent}}}, serde::Raw}, uuid::Uuid};

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
        Viewtype::Image | Viewtype::Gif => {
            if let Some((image_uri, image_info)) = upload_delta_image(da, client, &message).await? {
                let content = RoomMessageEventContent::new(MessageType::Image(
                    ImageMessageEventContent::plain(
                        message.get_filename().unwrap_or_default(),
                        image_uri,
                        Some(Box::new(image_info)),
                    ),
                ));
                send_delta_plain_message_content(client, room_id, &message, content).await?;
            }

            let content =
                RoomMessageEventContent::text_plain(message.get_text().unwrap_or_default());
            send_delta_plain_message_content(client, room_id, &message, content).await?;
        }
        Viewtype::Text => {
            let content =
                RoomMessageEventContent::text_plain(message.get_text().unwrap_or_default());
            send_delta_plain_message_content(client, room_id, &message, content).await?;
        }
        other => {
            RoomMessageEventContent::notice_plain(format!("(unhandled view type: {})", other));
        }
    };
    Ok(())
}

async fn upload_delta_image(
    da: &DeltaAppservice,
    client: &Client,
    message: &deltachat::message::Message,
) -> anyhow::Result<Option<(MxcUri, ImageInfo)>> {
    if let Some(mime) = message.get_filemime() {
        if let Some(filename) = message.get_file(&da.ctx) {
            let f = dc_open_file_std(&da.ctx, filename)?;
            let mut reader = BufReader::new(f);
            let response = client.upload(&mime.parse().unwrap(), &mut reader).await?;

            let mut image_info = ImageInfo::new();
            image_info.mimetype = Some(mime);
            image_info.width = Some(message.get_width().try_into().unwrap());
            image_info.height = Some(message.get_height().try_into().unwrap());
            image_info.size = Some(message.get_filebytes(&da.ctx).await.try_into().unwrap());
            return Ok(Some((response.content_uri, image_info)));
        }
    }
    Ok(None)
}

async fn send_delta_plain_message_content(
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
