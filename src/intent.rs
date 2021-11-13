use matrix_sdk::{
    ruma::{
        api::client::r0::message::send_message_event, events::MessageEventContent, serde::Raw,
        RoomId,
    },
    uuid::Uuid,
    Client, Result,
};

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
