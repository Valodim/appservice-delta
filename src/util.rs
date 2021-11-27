use matrix_sdk::{
    ruma::{api::client::r0::room::get_room_event, events::MessageEvent, EventId, RoomId},
    Client, Result,
};

use crate::types;

pub fn addr_to_localpart(addr: &str) -> String {
    // probably too dumb, but works for now
    addr.replace("@", "_at_").to_lowercase()
}

pub async fn get_delta_event(
    client: &Client,
    room_id: &RoomId,
    event_id: &EventId,
) -> Result<Option<MessageEvent<types::DeltaRoomEventContent>>> {
    let request = get_room_event::Request::new(room_id, event_id);
    let result = client.send(request, None).await?;
    Ok(result
        .event
        .deserialize_as::<MessageEvent<types::DeltaRoomEventContent>>()
        .ok())
}
