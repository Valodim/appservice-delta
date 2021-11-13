use std::path::Path;

use std::convert::TryFrom;

use log::info;

use deltachat::{
    chat::{Chat, ChatId},
    config::Config,
    contact::Contact,
    context::Context,
    message::{self, MsgId},
    EventType,
};

use matrix_sdk::{Client, event_handler::Ctx, room::{Joined, Room}, ruma::api::client::r0::room::create_room::Request as CreateRoomRequest, ruma::api::client::r0::room::{create_room::RoomPreset, get_room_event::Request as GetRoomEventRequest}, ruma::events::reaction::{Relation as ReactionRelation, SyncReactionEvent}, ruma::events::{reaction::ReactionEventContent, room::member::SyncRoomMemberEvent}, ruma::events::{room::member::MembershipState, MessageEvent}, ruma::{
        events::room::message::{
            MessageType, Relation as MessageRelation, RoomMessageEventContent,
            SyncRoomMessageEvent, TextMessageEventContent,
        },
        EventId,
    }, ruma::{RoomId, UserId}};

use matrix_sdk_appservice::{AppService, AppServiceRegistration, Result};
use types::DeltaMessageFields;

mod cmdline;
mod compat;
mod intent;
mod types;

#[derive(Debug, Clone)]
struct DeltaAppservice {
    ctx: Context,
    appservice: AppService,
}

impl DeltaAppservice {
    async fn new(dbfile: &Path, appservice: AppService) -> DeltaAppservice {
        log::info!("creating database {:?}", dbfile);
        let ctx = Context::new("appservice-delta".into(), dbfile.into(), 0)
            .await
            .expect("Failed to create context");
        DeltaAppservice { ctx, appservice }
    }

    async fn start(&self) {
        log::info!("------ RUN ------");
        self.ctx.start_io().await;
    }

    async fn stop(self) {
        log::info!("stopping");
        self.ctx.stop_io().await;
    }

    pub fn get_event_emitter(&self) -> deltachat::EventEmitter {
        self.ctx.get_event_emitter()
    }

    pub fn get_main_user(&self) -> Client {
        self.appservice.get_cached_client(None).unwrap()
    }

    pub async fn get_control_room_id(&self) -> Option<RoomId> {
        self.ctx
            .get_ui_config("ui.appservice.control_room_id")
            .await
            .unwrap()
            .map(|raw_id| RoomId::try_from(raw_id).unwrap())
    }

    pub async fn set_repl_active_chat_id(&self, chat_id: ChatId) {
        self.ctx
            .set_ui_config(
                "ui.appservice.repl_active_chat_id",
                Some(chat_id.to_u32().to_string().as_str()),
            )
            .await
            .unwrap();
    }

    pub async fn get_repl_active_chat_id(&self) -> ChatId {
        self.ctx
            .get_ui_config("ui.appservice.repl_active_chat_id")
            .await
            .unwrap()
            .and_then(|id_str| id_str.parse::<u32>().ok())
            .map(|raw_id| ChatId::new(raw_id))
            .unwrap_or_else(|| ChatId::new(0))
    }

    pub async fn get_control_room(&self) -> Option<Joined> {
        let control_room_id = self.get_control_room_id().await;
        let main_user = self.get_main_user();
        if let Some(control_room_id) = control_room_id {
            log::info!("control room: {}", control_room_id);
            let room = main_user.get_joined_room(&control_room_id);
            if room.is_none() {
                log::debug!("fetching control room state");
                compat::fetch_initial_room_state(&main_user, &control_room_id)
                    .await
                    .unwrap();
                let room = main_user.get_joined_room(&control_room_id);
                return room;
            }
            room
        } else {
            log::warn!("no control room configured!");
            None
        }
    }

    pub async fn get_config(&self, custom_key: &str) -> Option<String> {
        self.ctx
            .get_ui_config(&format!("ui.appservice.custom.{}", custom_key))
            .await
            .unwrap()
    }

    pub async fn set_config(&self, key: &str, value: Option<&str>) {
        self.ctx
            .set_ui_config(&format!("ui.appservice.custom.{}", key), value)
            .await
            .unwrap()
    }

    pub async fn clear_room_id_and_chat_id(&self, room_id: &RoomId, chat_id: ChatId) {
        self.set_config(
            &format!("chat_id_by_room_id.{}", room_id),
            None,
        )
        .await;
        self.set_config(
            &format!(
                "room_id_by_chat_id.{}",
                chat_id.to_u32().to_string().as_str()
            ),
            None,
        )
        .await;
    }

    pub async fn set_room_id_with_chat_id(&self, room_id: &RoomId, chat_id: ChatId) {
        self.set_config(
            &format!("chat_id_by_room_id.{}", room_id),
            Some(chat_id.to_u32().to_string().as_str()),
        )
        .await;
        self.set_config(
            &format!(
                "room_id_by_chat_id.{}",
                chat_id.to_u32().to_string().as_str()
            ),
            Some(room_id.as_str()),
        )
        .await;
    }

    pub async fn get_chat_id_by_room_id(&self, room_id: &RoomId) -> Option<ChatId> {
        self.get_config(&format!("chat_id_by_room_id.{}", room_id.as_str()))
            .await
            .map(|r| ChatId::new(r.parse::<u32>().unwrap()))
    }

    pub async fn get_room_id_by_chat_id(&self, chat_id: ChatId) -> Option<RoomId> {
        self.get_config(&format!(
            "room_id_by_chat_id.{}",
            chat_id.to_u32().to_string()
        ))
        .await
        .and_then(|r| RoomId::try_from(r).ok())
    }

    pub async fn get_email_user_by_id(&self, contact_id: u32) -> Result<(Contact, Client)> {
        let contact = Contact::load_from_db(&self.ctx, contact_id).await.unwrap();
        let client = self.get_email_user(&contact).await.unwrap();

        Ok((contact, client))
    }

    pub async fn get_email_user(&self, contact: &Contact) -> Result<Client> {
        let localpart = addr_to_localpart(contact.get_addr());

        // ignore result, if already registered
        if let Err(_) = self.appservice.register_virtual_user(&localpart).await {
            log::debug!("{}: user already registered, ignoring error", localpart)
        }

        let client = self.appservice.virtual_user_client(&localpart).await?;

        let desired_display_name = contact.get_name_n_addr();
        if client
            .display_name()
            .await?
            .map(|d| d != desired_display_name)
            .unwrap_or(true)
        {
            client.set_display_name(Some(&desired_display_name)).await?;
        }

        if let Some(control_room) = self.get_control_room().await {
            if client.get_invited_room(control_room.room_id()).is_none() {
                // ignore result, if already invited TODO improve with room sync
                if let Err(x) = control_room
                    .invite_user_by_id(&client.user_id().await.unwrap())
                    .await
                {
                    log::error!("{}", x);
                }
            }
            if client.get_joined_room(control_room.room_id()).is_none() {
                // ignore result, if already joined TODO improve with room sync
                if let Err(x) = client.join_room_by_id(control_room.room_id()).await {
                    log::error!("{}", x);
                }
            }
        }

        Ok(client)
    }
}

async fn handle_room_member_event(
    da: DeltaAppservice,
    room: Room,
    ev: SyncRoomMemberEvent,
) -> Result<()> {
    let user_id = UserId::try_from(ev.state_key.as_str())?;
    let is_main_user = user_id.localpart() == da.appservice.registration().sender_localpart;
    let is_appservice_user = da.appservice.user_id_is_in_namespace(user_id)?;

    if is_main_user && ev.content.membership == MembershipState::Invite {
        return handle_room_main_user_invited(da, room).await;
    } else if !is_appservice_user && ev.content.membership == MembershipState::Leave {
        return handle_room_user_leave(da, room).await;
    }

    Ok(())
}

async fn handle_room_main_user_invited(
    da: DeltaAppservice,
    room: Room,
) -> Result<()> {
    let client = da.appservice.get_cached_client(None)?;

    client.join_room_by_id(room.room_id()).await?;
    da.ctx
        .set_ui_config(
            "ui.appservice.control_room_id",
            Some(&room.room_id().to_string()),
        )
        .await
        .unwrap();

    Ok(())
}

async fn handle_room_user_leave(
    da: DeltaAppservice,
    room: Room,
) -> Result<()> {
    if let Some(chat_id) = da.get_chat_id_by_room_id(room.room_id()).await {
        da.clear_room_id_and_chat_id(room.room_id(), chat_id).await;
        clear_room(&da.appservice, room).await.unwrap();
    }

    Ok(())
}

async fn clear_room(
    _appservice: &AppService,
    _room: Room,
) -> Result<()> {
    // TODO fix
    /*
    for m in room.joined_members().await? {
        if appservice.user_id_is_in_namespace(m.user_id())? {
            if let Ok(client) = appservice.virtual_user_client(m.user_id().localpart()).await {
                let request = leave_room::Request::new(room.room_id());
                client.send(request, None).await.unwrap();
            }
        }
    }
    */

    Ok(())
}

async fn handle_room_message(
    da: DeltaAppservice,
    room: Room,
    event: SyncRoomMessageEvent,
) -> Result<()> {
    // ignore our own messages
    if event.sender == da.get_main_user().user_id().await.unwrap()
        || da
            .appservice
            .user_id_is_in_namespace(&event.sender)
            .unwrap()
    {
        return Ok(());
    }

    let msg_body = match &event.content.msgtype {
        MessageType::Text(TextMessageEventContent { body, .. }) => body.to_owned(),
        _ => return Ok(()),
    };

    if let Room::Joined(room) = room {
        let control_room_id = da.get_control_room_id().await;
        if let Some(MessageRelation::Reply { in_reply_to }) = event.content.relates_to {
            return handle_control_room_message_reply(da, room, msg_body, in_reply_to.event_id).await;
        } else if Some(room.room_id()) == control_room_id.as_ref() {
            return handle_control_room_message(da, room, msg_body).await;
        } else {
            return handle_chat_room_message(da, room, msg_body).await;
        }
    }
    Ok(())
}

async fn handle_chat_room_message(
    da: DeltaAppservice,
    room: Joined,
    msg_body: String,
) -> Result<()> {
    if let Some(chat_id) = da.get_chat_id_by_room_id(room.room_id()).await {
        deltachat::chat::send_text_msg(&da.ctx, chat_id, msg_body)
            .await
            .unwrap();
    }
    Ok(())
}

async fn handle_control_room_message(
    da: DeltaAppservice,
    room: Joined,
    msg_body: String,
) -> Result<()> {
    let x = msg_body.splitn(2, ' ').collect::<Vec<_>>();
    let cmd = x.get(0).map(|s| *s).unwrap_or("");
    let args = x.get(1).map(|s| *s).unwrap_or("");

    match cmd {
        "repl" => handle_control_room_command_repl(da, room, args).await,
        "configure" => handle_control_room_command_configure(da, room, args).await,
        _ => {
            let msg = RoomMessageEventContent::text_plain(format!("Unknown command {}. Commands:\n - repl\n - configure", cmd));
            room.send(msg, None).await?;
            Ok(())
        }
    }
}

async fn handle_control_room_command_configure(
    da: DeltaAppservice,
    room: Joined,
    arguments: &str,
) -> Result<()> {
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
    msg_body: String,
    reply_event_id: EventId,
) -> Result<()> {
    let client = da.get_main_user();
    let delta_fields = match get_delta_event(&client, room.room_id(), &reply_event_id).await? {
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
    deltachat::chat::send_text_msg(&da.ctx, chat_id, msg_body)
        .await
        .unwrap();

    Ok(())
}

async fn handle_room_reaction_event(
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
                    ReactionRelation {
                        event_id: react_event_id,
                        emoji,
                        ..
                    },
                ..
            },
        sender: react_sender,
        ..
    } = event;

    let delta_event = get_delta_event(&da.get_main_user(), room.room_id(), &react_event_id).await?;
    if let Some(MessageEvent {
        content:
            types::DeltaRoomEventContent {
                delta_fields: Some(DeltaMessageFields { chat_id, .. }),
            },
        sender: original_sender,
        ..
    }) = delta_event {
        let original_chat_id = ChatId::new(chat_id);
        match emoji.chars().next().unwrap() {
            '👍' => return handle_control_room_reaction_thumbsup(da, room, react_sender, original_sender, original_chat_id).await,
            '👎' => return handle_control_room_reaction_thumbsdown(da, room, react_sender, original_sender, original_chat_id).await,
            _ => log::warn!("Ignoring unknown reaction {}", emoji),
        }
    }
    Ok(())
}

async fn handle_control_room_reaction_thumbsup(
    da: DeltaAppservice,
    room: Joined,
    react_sender: UserId,
    original_sender: UserId,
    original_chat_id: ChatId,
) -> Result<()> {
    if da.get_room_id_by_chat_id(original_chat_id).await.is_some() {
        let msg = RoomMessageEventContent::text_plain("Chat is already a room.");
        room.send(msg, None).await?;
        return Ok(());
    }

    let client = da
        .appservice
        .virtual_user_client(original_sender.localpart())
        .await?;

    original_chat_id.accept(&da.ctx).await.unwrap();

    let chat = Chat::load_from_db(&da.ctx, original_chat_id).await.unwrap();

    let invite_user_ids = vec![react_sender];
    let mut r = CreateRoomRequest::new();
    r.invite = &invite_user_ids;
    r.topic = Some(chat.get_name());
    r.preset = Some(RoomPreset::TrustedPrivateChat);
    r.is_direct = true;
    let response = client.create_room(r).await.unwrap();
    da.set_room_id_with_chat_id(&response.room_id, chat.get_id())
        .await;

    Ok(())
}

async fn handle_control_room_reaction_thumbsdown(
    da: DeltaAppservice,
    _room: Joined,
    _react_sender: UserId,
    _original_sender: UserId,
    original_chat_id: ChatId,
) -> Result<()> {
    original_chat_id.block(&da.ctx).await.unwrap();

    /* TODO handle more?
    let client = da
        .appservice
        .virtual_user_client(original_sender.localpart())
        .await?;

    let chat = Chat::load_from_db(&da.ctx, original_chat_id).await.unwrap();

    let invite_user_ids = vec![react_sender];
    let mut r = CreateRoomRequest::new();
    r.invite = &invite_user_ids;
    r.topic = Some(chat.get_name());
    r.preset = Some(RoomPreset::TrustedPrivateChat);
    r.is_direct = true;
    let response = client.create_room(r).await.unwrap();
    da.set_room_id_with_chat_id(&response.room_id, chat.get_id())
        .await;
    */

    Ok(())
}

async fn get_delta_event(
    client: &Client,
    room_id: &RoomId,
    event_id: &EventId,
) -> Result<Option<MessageEvent<types::DeltaRoomEventContent>>> {
    let request = GetRoomEventRequest::new(room_id, event_id);
    let result = client.send(request, None).await?;
    Ok(result
        .event
        .deserialize_as::<MessageEvent<types::DeltaRoomEventContent>>()
        .ok())
}

fn addr_to_localpart(addr: &str) -> String {
    // probably too dumb, but works for now
    addr.replace("@", "_at_").to_lowercase()
}

async fn callback_delta_event(da: DeltaAppservice, event: EventType) {
    match event {
        EventType::IncomingMsg { chat_id, msg_id } => {
            log::info!("{:?}", event);
            handle_chat_incoming_message(da, chat_id, msg_id)
                .await
                .unwrap();
        }
        // TODO read markers
        // EventType::MsgDelivered { chat_id, msg_id } => {
        //  log::info!("{:?}", event);
        // }
        EventType::MsgFailed { chat_id, msg_id } => {
            log::info!("{:?}", event);
            handle_chat_failed_message(da, chat_id, msg_id)
                .await
                .unwrap();
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

async fn handle_chat_incoming_message(
    da: DeltaAppservice,
    chat_id: ChatId,
    msg_id: MsgId,
) -> Result<()> {
    if let Some(control_room_id) = da.get_control_room_id().await {
        let message = message::Message::load_from_db(&da.ctx, msg_id)
            .await
            .unwrap();
        // let text = message::get_msg_info(&delta.ctx, msg_id).await.unwrap();
        let (_, user_client) = da.get_email_user_by_id(message.get_from_id()).await?;

        let msg = RoomMessageEventContent::text_plain(message.get_text().unwrap());
        let content = types::DeltaRoomMessageEventContent {
            delta_fields: Some(types::DeltaMessageFields {
                external_url: Some(message.get_rfc724_mid().to_owned()),
                msg_id: msg_id.to_u32(),
                chat_id: chat_id.to_u32(),
            }),
            msg,
        };

        let room_id = da
            .get_room_id_by_chat_id(chat_id)
            .await
            .unwrap_or_else(|| control_room_id);

        intent::send_message(&user_client, &room_id, content).await?
    } else {
        log::error!("no control room registered");
    }
    Ok(())
}

async fn handle_chat_failed_message(
    da: DeltaAppservice,
    chat_id: ChatId,
    msg_id: MsgId,
) -> Result<()> {
    let msg = RoomMessageEventContent::text_plain("Sending message failed");

    let message = message::Message::load_from_db(&da.ctx, msg_id)
        .await
        .unwrap();
    let (_, user_client) = da.get_email_user_by_id(message.get_from_id()).await?;

    let control_room_id = da.get_control_room_id().await;
    let room_id = da
        .get_room_id_by_chat_id(chat_id)
        .await
        .or(control_room_id)
        .unwrap();

    intent::send_message(&user_client, &room_id, msg).await?;
    Ok(())
}

#[async_std::main]
pub async fn main() -> Result<(), Box<dyn std::error::Error>> {
    std::env::set_var("RUST_LOG", "matrix_sdk=info,matrix_sdk_appservice=info,hyper=warn,pgp=warn,appservice_email_delta=debug,deltachat=debug,debug");
    pretty_env_logger::try_init_timed().ok();

    let homeserver_url = "http://localhost:8008";
    let server_name = "localhost";
    let registration =
        AppServiceRegistration::try_from_yaml_file("./appservice-registration-delta.yaml")?;
    let appservice = AppService::new(homeserver_url, server_name, registration).await?;

    let dbfile = std::path::PathBuf::from(".").join("data/db.sqlite");
    let da = DeltaAppservice::new(&dbfile, appservice.clone()).await;

    appservice
        .register_event_handler_context(da.clone())?
        .register_event_handler(
            move |event: SyncRoomMemberEvent, room: Room, Ctx(da): Ctx<DeltaAppservice>| {
                info!("event: {:?}", event);
                handle_room_member_event(da, room, event)
            },
        )
        .await?
        .register_event_handler(
            move |event: SyncRoomMessageEvent, room: Room, Ctx(da): Ctx<DeltaAppservice>| {
                info!("event: {:?}", event);
                handle_room_message(da, room, event)
            },
        )
        .await?
        .register_event_handler(
            move |event: SyncReactionEvent, room: Room, Ctx(da): Ctx<DeltaAppservice>| {
                info!("event: {:?}", event);
                handle_room_reaction_event(da, room, event)
            },
        )
        .await?;

    let delta2 = da.clone();
    let events = da.get_event_emitter();
    let events_spawn = async_std::task::spawn(async move {
        while let Some(event) = events.recv().await {
            callback_delta_event(delta2.clone(), event.typ.clone()).await;
        }
    });

    if da.ctx.is_configured().await? {
        da.start().await;
    }

    let (host, port) = appservice.registration().get_host_and_port()?;
    // this blocks until we are killed
    appservice.run(host, port).await?;

    da.stop().await;
    events_spawn.await;

    Ok(())

    /*

    for i in 0..1 {
        log::info!("sending message {}", i);
        chat::send_text_msg(&ctx, chat_id, format!("Hi, here is my {}nth message!", i))
            .await
            .unwrap();
    }

    log::info!("fetching chats..");
    let chats = Chatlist::try_load(&ctx, 0, None, None).await.unwrap();

    for i in 0..chats.len() {
        let msg = Message::load_from_db(&ctx, chats.get_msg_id(i).unwrap().unwrap())
            .await
            .unwrap();
        log::info!("[{}] msg: {:?}", i, msg);
    }
    */
}
