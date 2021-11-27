use matrix_sdk_appservice::Result;
use serde::Deserialize;

use matrix_sdk::{Client, ruma::{RoomId, api::client::r0::{state::get_state_events, sync::sync_events}, serde::Raw}};

use log;

pub async fn fetch_initial_room_state(client: &Client, room_id: &RoomId) -> Result<()> {
    let state_request = get_state_events::Request::new(&room_id);
    let state_response = client.send(state_request, None).await?;

    #[derive(Debug, Deserialize)]
    struct EventDeHelper {
        room_id: Option<RoomId>,
    }

    let mut response = sync_events::Response::new("batch".to_owned());

    for raw_event in state_response.room_state {
        let helper = raw_event.deserialize_as::<EventDeHelper>()?;
        let event_json = Raw::into_json(raw_event);

        if let Some(room_id) = helper.room_id {
            let join = response.rooms.join.entry(room_id).or_default();
            join.state.events.push(Raw::from_json(event_json));
        } else {
            log::warn!("Event without room_id: {}", event_json);
        }
    }

    log::trace!("Updating state with sync_response: {:?}", &response);

    client.process_sync(response).await?;

    Ok(())
}
