use std::sync::Arc;
use std::{path::Path, time::Duration};

use std::convert::{TryFrom, TryInto};

use async_std::task;
use deltachat::constants::DC_CONTACT_ID_SELF;
use log::info;

use deltachat::{chat::ChatId, config::Config, contact::Contact, context::Context, message::MsgId};

use matrix_sdk::{
    config::ClientConfig,
    event_handler::Ctx,
    room::{Joined, Room},
    ruma::{
        events::{
            reaction::SyncReactionEvent, room::member::SyncRoomMemberEvent,
            room::message::SyncRoomMessageEvent, room::topic::SyncRoomTopicEvent,
        },
        EventId, RoomId, UserId,
    },
    Client,
};

use matrix_sdk_appservice::{AppService, AppServiceRegistration, Result};

use dashmap::DashMap;

mod cmdline;
mod delta_events;
mod matrix_events;
mod matrix_intents;
mod types;
mod util;

#[derive(Debug, Clone)]
pub struct DeltaAppservice {
    ctx: Context,
    appservice: AppService,
    client_cache: Arc<DashMap<u32, Client>>,
    event_message_cache: Arc<DashMap<MsgId, EventId>>,
}

impl DeltaAppservice {
    async fn new(dbfile: &Path, appservice: AppService) -> DeltaAppservice {
        log::info!("creating database {:?}", dbfile);
        let ctx = Context::new("appservice-delta".into(), dbfile.into(), 0)
            .await
            .expect("Failed to create context");
        let client_cache = Arc::new(DashMap::new());
        let event_message_cache = Arc::new(DashMap::new());
        DeltaAppservice {
            ctx,
            appservice,
            client_cache,
            event_message_cache,
        }
    }

    async fn start(&self) {
        self.ctx.start_io().await;
    }

    async fn stop(self) {
        self.ctx.stop_io().await;
    }

    pub fn get_event_emitter(&self) -> deltachat::EventEmitter {
        self.ctx.get_event_emitter()
    }

    pub fn get_main_user(&self) -> Client {
        self.appservice.get_cached_client(None).unwrap()
    }

    pub async fn get_owner_user_id(&self) -> UserId {
        self.ctx
            .get_ui_config("ui.appservice.owner_user_id")
            .await
            .unwrap()
            .unwrap()
            .try_into()
            .unwrap()
    }

    pub async fn set_owner_user_id(&self, owner_user_id: &UserId) -> anyhow::Result<()> {
        self.ctx
            .set_ui_config(
                "ui.appservice.owner_user_id",
                Some(&owner_user_id.to_string()),
            )
            .await
    }

    pub async fn get_control_room_id(&self) -> Option<RoomId> {
        self.ctx
            .get_ui_config("ui.appservice.control_room_id")
            .await
            .unwrap()
            .map(|raw_id| RoomId::try_from(raw_id).unwrap())
    }

    pub async fn set_control_room_id(&self, control_room_id: &RoomId) -> anyhow::Result<()> {
        self.ctx
            .set_ui_config(
                "ui.appservice.control_room_id",
                Some(&control_room_id.to_string()),
            )
            .await
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
            main_user.get_joined_room(&control_room_id)
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
        self.set_config(&format!("chat_id_by_room_id.{}", room_id), None)
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

    pub async fn get_email_user_by_id(
        &self,
        contact_id: u32,
        refresh: bool,
    ) -> Result<(Contact, Client)> {
        let contact = Contact::load_from_db(&self.ctx, contact_id).await.unwrap();
        let client = self.get_email_user(&contact, refresh).await.unwrap();

        Ok((contact, client))
    }

    pub async fn get_email_user(&self, contact: &Contact, refresh: bool) -> anyhow::Result<Client> {
        if !refresh {
            if let Some(client) = self.client_cache.get(&contact.id) {
                return Ok(client.clone());
            }
        }

        let localpart = util::addr_to_localpart(contact.get_addr());

        // ignore result, if already registered
        if let Err(_) = self.appservice.register_virtual_user(&localpart).await {
            log::debug!("{}: user already registered, ignoring error", localpart)
        }

        let client = self.appservice.virtual_user_client(&localpart).await?;

        let desired_display_name = match contact.get_id() {
            DC_CONTACT_ID_SELF => self
                .ctx
                .get_config(Config::Displayname)
                .await?
                .unwrap_or("Me".to_owned()),
            _ => contact.get_display_name().to_owned(),
        };
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
                    log::debug!("{}", x);
                }
            }
            if client.get_joined_room(control_room.room_id()).is_none() {
                // ignore result, if already joined TODO improve with room sync
                if let Err(x) = client.join_room_by_id(control_room.room_id()).await {
                    log::debug!("{}", x);
                }
            }
        }

        self.client_cache.insert(contact.id, client.clone());

        Ok(client)
    }
}

#[async_std::main]
pub async fn main() -> Result<(), Box<dyn std::error::Error>> {
    std::env::set_var("RUST_LOG", "matrix_sdk=info,matrix_sdk_appservice=info,hyper=warn,pgp=warn,appservice_email_delta=debug,sled=info,deltachat=debug,debug");
    pretty_env_logger::try_init_timed().ok();

    let data_path = std::path::PathBuf::from("./data");

    let appservice = {
        let homeserver_url = "http://localhost:8008";
        let server_name = "localhost";
        let registration =
            AppServiceRegistration::try_from_yaml_file("./appservice-registration-delta.yaml")?;
        let config = ClientConfig::new().store_path(data_path.join("appservice"));
        AppService::new_with_config(homeserver_url, server_name, registration, config).await?
    };

    let da = {
        let dbfile = data_path.join("delta/db.sqlite");
        DeltaAppservice::new(&dbfile, appservice.clone()).await
    };

    appservice
        .register_event_handler_context(da.clone())?
        .register_event_handler(
            move |event: SyncRoomMemberEvent, room: Room, Ctx(da): Ctx<DeltaAppservice>| {
                info!("event: {:?}", event);
                matrix_events::handle_room_member_event(da, room, event)
            },
        )
        .await?
        .register_event_handler(
            move |event: SyncRoomMessageEvent, room: Room, Ctx(da): Ctx<DeltaAppservice>| {
                info!("event: {:?}", event);
                matrix_events::handle_room_message(da, room, event)
            },
        )
        .await?
        .register_event_handler(
            move |event: SyncReactionEvent, room: Room, Ctx(da): Ctx<DeltaAppservice>| {
                info!("event: {:?}", event);
                matrix_events::handle_room_reaction_event(da, room, event)
            },
        )
        .await?
        .register_event_handler(
            move |event: SyncRoomTopicEvent, room: Room, Ctx(da): Ctx<DeltaAppservice>| {
                info!("event: {:?}", event);
                matrix_events::handle_room_topic_event(da, room, event)
            },
        )
        .await?;

    let events_spawn = {
        let da = da.clone();
        let events = da.get_event_emitter();
        async_std::task::spawn(async move {
            while let Some(event) = events.recv().await {
                delta_events::callback_delta_event(&da, event.typ.clone()).await;
            }
        })
    };

    if da.ctx.is_configured().await? {
        da.start().await;
    }

    {
        let da = da.clone();
        task::spawn(async move {
            loop {
                task::sleep(Duration::from_secs(5 * 60)).await;
                da.ctx.maybe_network().await;
            }
        });
    }

    let (host, port) = appservice.registration().get_host_and_port()?;
    // this blocks until we are killed
    appservice.run(host, port).await?;

    da.stop().await;
    events_spawn.await;

    Ok(())
}
