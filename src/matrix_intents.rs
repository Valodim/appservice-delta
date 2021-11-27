use std::convert::TryInto;
use std::io::BufReader;

use deltachat::{
    chat::{Chat, ChatId, ChatItem},
    constants::{Chattype, Viewtype, DC_CONTACT_ID_SELF},
    contact::Contact,
    dc_tools::dc_open_file_std,
    message,
};

use matrix_sdk::{
    room::{Joined, Room},
    ruma::{
        api::client::r0::{
            membership::leave_room,
            room::create_room::{self, RoomPreset},
        },
        api::client::r0::{message::send_message_event, room::create_room::CreationContent},
        events::{
            room::{
                avatar::RoomAvatarEventContent,
                create::RoomType,
                message::{
                    FileInfo, FileMessageEventContent, ImageMessageEventContent, MessageType,
                    RoomMessageEventContent,
                },
                ImageInfo,
            },
            space::child::SpaceChildEventContent,
            MessageEventContent,
        },
        serde::Raw,
        EventId, MxcUri, RoomId,
    },
    uuid::Uuid,
    Client, Result,
};

use crate::{types, compat, DeltaAppservice};

pub async fn create_space(da: &DeltaAppservice, avatar_uri: Option<MxcUri>) -> Result<RoomId> {
    let invite_user_ids = vec![da.get_owner_user_id().await];

    let mut r = create_room::Request::new();
    r.preset = Some(RoomPreset::TrustedPrivateChat);
    r.creation_content = {
        let mut cc = CreationContent::new();
        cc.room_type = Some(RoomType::Space);
        Some(Raw::new(&cc).unwrap())
    };
    r.name = Some("Delta Appservice".try_into().unwrap());
    r.invite = invite_user_ids.as_slice();

    let main_user = da.get_main_user();
    let response = main_user.create_room(r).await.unwrap();
    compat::fetch_initial_room_state(&main_user, &response.room_id).await.unwrap();

    let space_room = main_user.get_joined_room(&response.room_id).unwrap();
    let control_room_id = da.get_control_room_id().await.unwrap();
    let space_child_event_content = {
        let mut scec = SpaceChildEventContent::new();
        let server_name = main_user.user_id().await.unwrap().server_name().to_owned();
        scec.via = Some(vec![server_name]);
        scec.suggested = Some(true);
        scec
    };
    space_room
        .send_state_event(space_child_event_content, control_room_id.as_str())
        .await?;

    let mut avatar_event = RoomAvatarEventContent::new();
    avatar_event.url = avatar_uri;
    space_room.send_state_event(avatar_event, "").await?;

    Ok(response.room_id)
}

pub async fn create_room_for_chat(
    da: &DeltaAppservice,
    _control_room: Joined,
    chat_id: ChatId,
) -> anyhow::Result<RoomId> {
    let chat = Chat::load_from_db(&da.ctx, chat_id).await.unwrap();

    let mut invite_user_ids = Vec::new();
    let mut invite_clients = Vec::new();
    for cid in deltachat::chat::get_chat_contacts(&da.ctx, chat.get_id())
        .await
        .unwrap()
        .into_iter()
    {
        let contact = Contact::get_by_id(&da.ctx, cid).await.unwrap();
        let client = da.get_email_user(&contact, false).await.unwrap();
        invite_user_ids.push(client.user_id().await.unwrap());
        invite_clients.push(client);
    }
    invite_user_ids.push(da.get_owner_user_id().await);

    let mut r = create_room::Request::new();
    r.preset = Some(RoomPreset::TrustedPrivateChat);
    if chat.get_type() == Chattype::Single {
        r.is_direct = true;
        r.name = Some(chat.get_name().try_into().unwrap());

        // the self contact is not in chats with type single, so we add it manually
        let contact = Contact::get_by_id(&da.ctx, DC_CONTACT_ID_SELF)
            .await
            .unwrap();
        let client = da.get_email_user(&contact, false).await.unwrap();
        invite_user_ids.push(client.user_id().await.unwrap());
        invite_clients.push(client);
    } else {
        r.name = Some(chat.get_name().try_into().unwrap());
        r.topic = Some(chat.get_name());
    }
    r.invite = invite_user_ids.as_slice();

    let create_client = da.get_main_user();
    let response = create_client.create_room(r).await.unwrap();

    for client in invite_clients {
        client.join_room_by_id(&response.room_id).await.unwrap();
    }

    chat.get_id().accept(&da.ctx).await.unwrap();
    da.set_room_id_with_chat_id(&response.room_id, chat.get_id())
        .await;

    let space_room = da.get_space_room().await.unwrap();
    let space_child_event_content = {
        let mut scec = SpaceChildEventContent::new();
        let server_name = create_client
            .user_id()
            .await
            .unwrap()
            .server_name()
            .to_owned();
        scec.via = Some(vec![server_name]);
        scec
    };
    space_room
        .send_state_event(space_child_event_content, response.room_id.as_str())
        .await?;

    backfill_chat(da, &response.room_id, chat.get_id())
        .await
        .unwrap();

    Ok(response.room_id)
}

async fn backfill_chat(
    da: &DeltaAppservice,
    room_id: &RoomId,
    chat_id: ChatId,
) -> anyhow::Result<()> {
    let msglist = deltachat::chat::get_chat_msgs(&da.ctx, chat_id, 0, None)
        .await
        .unwrap();
    for msg in msglist {
        if let ChatItem::Message { msg_id } = msg {
            let msg = message::Message::load_from_db(&da.ctx, msg_id)
                .await
                .unwrap();
            let (_, client) = da.get_email_user_by_id(msg.get_from_id(), false).await?;
            send_delta_message(&da, &client, &room_id, msg).await?
        }
    }

    Ok(())
}

pub async fn clear_room(da: &DeltaAppservice, room: Room) -> anyhow::Result<()> {
    for m in room.joined_members().await? {
        if da.appservice.user_id_is_in_namespace(m.user_id())? {
            if let Ok(client) = da
                .appservice
                .virtual_user_client(m.user_id().localpart())
                .await
            {
                let request = leave_room::Request::new(room.room_id());
                client.send(request, None).await.unwrap();
            }
        }
    }

    let main_user = da.get_main_user();
    let request = leave_room::Request::new(room.room_id());
    main_user.send(request, None).await.unwrap();

    Ok(())
}

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
        log::warn!(
            "User {} not joined in room {}, falling back to send_message",
            client.user_id().await.unwrap(),
            room_id
        );
        send_message(client, room_id, content).await?
    };

    da.event_message_cache
        .insert(message.get_id().clone(), event_id);

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
