use serde::{Deserialize, Serialize};

use matrix_sdk::ruma::events::macros::EventContent;
use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;

#[derive(Clone, Debug, Deserialize, Serialize, EventContent)]
#[ruma_event(type = "m.room.message", kind = Message)]
pub struct DeltaRoomMessageEventContent {
    pub delta_fields: Option<DeltaMessageFields>,

    #[serde(flatten)]
    pub msg: RoomMessageEventContent,
}

#[derive(Clone, Debug, Deserialize, Serialize, EventContent)]
#[ruma_event(type = "m.room.message", kind = Message)]
pub struct DeltaRoomEventContent {
    pub delta_fields: Option<DeltaMessageFields>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DeltaMessageFields {
    pub external_url: Option<String>,
    pub msg_id: u32,
    pub chat_id: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, EventContent)]
#[ruma_event(type = "delta.bridged", kind = State)]
pub struct DeltaChatRoomEventContent {
    pub delta_fields: Option<DeltaMessageFields>,
}

