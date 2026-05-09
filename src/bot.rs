use crate::{
	Config, State, console::MessageCommandResult,
	crawler::{find_and_join_rooms, log_message},
	tweak::{TweakMode, TweakerConfig, Keyword}
};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use serde::Deserialize;
use tokio::{sync::Mutex, time::{sleep, Duration}};
use matrix_sdk::{
	Client, Room, RoomState, SessionMeta,
	config::SyncSettings, event_handler::Ctx,
	deserialized_responses::TimelineEventKind,
	reqwest::{Client as ReqwestClient, Error as ReqwestError},
	authentication::{SessionTokens, matrix::MatrixSession},
	ruma::{
		OwnedDeviceId, OwnedEventId, OwnedRoomOrAliasId, UserId,
		room::JoinRuleSummary,
		events::{
			reaction::{ReactionEventContent, OriginalSyncReactionEvent},
			relation::Annotation,
			room::{
				member::{
					MembershipState, OriginalSyncRoomMemberEvent,
					StrippedRoomMemberEvent
				},
				message::{
					AddMentions, ForwardThread, OriginalSyncRoomMessageEvent,
					Relation, RoomMessageEventContent
				}
			}
		}
	}
};
use rand::{RngExt, rng};
use chrono::{DateTime, Utc, TimeDelta};

const DEFAULT_USER_AGENT: &str = "Tweaker; +https://git.calitabby.net/crispycat/tweaker";

pub(crate) fn http_client(config: &Config) -> Result<ReqwestClient, ReqwestError> {
	let ua = config.user_agent.as_deref()
		.unwrap_or(DEFAULT_USER_AGENT);

	let mut http = ReqwestClient::builder()
		.user_agent(ua);

	if let Some(timeout) = config.request_timeout {
		http = http.timeout(Duration::from_secs(timeout));
	}

	if let Some(timeout) = config.connect_timeout {
		http = http.connect_timeout(Duration::from_secs(timeout));
	}

	http.build()
}

#[derive(Deserialize, Clone)]
pub(crate) struct Account {
	server_url: Option<String>,
	mxid: String,
	password: Option<String>,
	token: Option<String>,
	device_id: Option<String>
}

impl Account {
	pub(crate) async fn login(&self, config: &Config) -> crate::Result<Client> {
		let mxid = UserId::parse(&self.mxid)?;
		tracing::info!("Logging in {mxid}");

		let http = http_client(config)?;
		let mut client = Client::builder()
			.http_client(http)
			.server_name(mxid.server_name());
		if let Some(ref url) = self.server_url {
			client = client.homeserver_url(url);
		}
		let client = client.build().await?;

		if let Some(ref password) = self.password {
			client.matrix_auth()
				.login_username(mxid, password)
				.send().await?;
		}
		else if let Some(ref token) = self.token {
			let device_id = self.device_id
				.clone().ok_or("Missing device id")?;
			let session = MatrixSession {
				meta: SessionMeta {
					user_id: mxid,
					device_id: OwnedDeviceId::from(device_id)
				},
				tokens: SessionTokens {
					access_token: token.to_string(),
					refresh_token: None
				}
			};
			client.restore_session(session).await?;
		}
		else {
			return Err(
				format!("No password or token supplied for {}", self.mxid).into()
			);
		}

		Ok(client)
	}
}

pub(crate) fn find_keyword(text: &str, keywords: &Vec<Keyword>) -> Option<Keyword> {
	let text = text.to_lowercase();
	for keyword in keywords {
		if text.contains(&keyword.keyword().to_lowercase()) {
			return Some(keyword.clone());
		}
	}
	None
}

pub(crate) async fn is_reply_to_self(
	room: &Room, message: &OriginalSyncRoomMessageEvent, client_id: String
) -> bool {
	if let Some(Relation::Reply{ref in_reply_to}) = message.content.relates_to
	&& let Ok(event) = room.load_or_fetch_event(&in_reply_to.event_id, None).await
	&& let TimelineEventKind::PlainText {event} = event.kind {
		return event.get_field::<String>("sender").ok() == Some(Some(client_id));
	}

	false
}

pub(crate) struct MessageProps {
	pub keyword: Option<Keyword>,
	pub reply: bool,
	pub timestamp: DateTime<Utc>,
	pub now: DateTime<Utc>
}

impl MessageProps {
	pub(crate) async fn for_message(
		room: &Room, message: &OriginalSyncRoomMessageEvent,
		client_id: String, keywords: &Vec<Keyword>
	) -> Self {
		let now = Utc::now();
		let ts = message.origin_server_ts.as_secs().into();
		let ts = DateTime::from_timestamp_secs(ts).unwrap_or_default();

		let body = message.content.body();
		if body.is_empty() {
			return Self {
				keyword: None,
				reply: false,
				timestamp: ts,
				now
			};
		}

		Self {
			keyword: find_keyword(body, keywords),
			reply: is_reply_to_self(room, message, client_id).await,
			timestamp: ts,
			now
		}
	}
}

#[allow(clippy::large_enum_variant)]
pub(crate) enum Response {
	None,
	Message(RoomMessageEventContent),
	Reaction(ReactionEventContent)
}

impl Response {
	pub(crate) fn message(text: &str, reply: Option<&OriginalSyncRoomMessageEvent>) -> Self {
		let mut evc = RoomMessageEventContent::text_plain(text);
		if let Some(message) = reply {
			evc = evc.make_reply_to(
				message, ForwardThread::No, AddMentions::Yes
			);
		}
		Self::Message(evc)
	}

	pub(crate) fn reaction(event_id: OwnedEventId, text: String) -> Self {
		let evc = ReactionEventContent::new(
			Annotation::new(event_id, text)
		);
		Self::Reaction(evc)
	}

	async fn send(self, room: &Room) -> crate::Result<()> {
		match self {
			Self::None => (),
			Self::Message(evc) => {room.send(evc).await?;},
			Self::Reaction(evc) => {room.send(evc).await?;},
		}
		Ok(())
	}

	async fn delay_type_send(self, config: &TweakerConfig, room: &Room)
	-> crate::Result<()> {
		if matches!(self, Self::None) {
			return Ok(());
		}

		let delay = rng().random_range(config.delay_min..config.delay_max);
		tracing::debug!("Delaying for {delay}s");
		sleep(Duration::from_secs(delay)).await;

		let mut is_message = false;
		if let Self::Message(ref message) = self {
			is_message = true;
			let type_dur = config.typing_time * (message.body().len() as f64);
			tracing::debug!("Typing for {type_dur}s");
			room.typing_notice(true).await?;
			sleep(Duration::from_secs(type_dur as u64)).await;
		}

		self.send(room).await?;
		if is_message {
			room.typing_notice(false).await?;
		}
		Ok(())
	}
}

pub(crate) struct ResponseQueue {
	paused: Arc<AtomicBool>,
	queue: Arc<Mutex<Vec<(Response, Room)>>>
}

impl ResponseQueue {
	pub(crate) fn new() -> Self {
		Self {
			paused: Arc::new(AtomicBool::new(false)),
			queue: Arc::new(Mutex::new(vec![]))
		}
	}

	pub(crate) async fn len(&self) -> usize {
		let queue = self.queue.lock().await;
		queue.len()
	}

	pub(crate) async fn add(&self, response: Response, room: Room) {
		let mut queue = self.queue.lock().await;
		tracing::debug!("adding response to queue (new length: {})", queue.len() + 1);
		queue.push((response, room));
	}

	pub(crate) async fn purge(&self) {
		let mut queue = self.queue.lock().await;
		tracing::info!("purging queue");
		*queue = vec![];
	}

	pub(crate) fn is_paused(&self) -> bool {
		self.paused.load(Ordering::Relaxed)
	}

	pub(crate) fn set_paused(&self, paused: bool) {
		self.paused.store(paused, Ordering::Relaxed);
	}

	pub(crate) async fn send_one(&self, config: &TweakerConfig)
	-> crate::Result<()> {
		let mut queue = self.queue.lock().await;
		let item = queue.pop();
		let len = queue.len();
		drop(queue);

		if let Some((response, room)) = item {
			tracing::debug!("sending response from queue (new length: {len})");
			return response.delay_type_send(config, &room).await;
		}

		Ok(())
	}

	pub(crate) async fn send_loop(&self, state: State) {
		let queue = self.clone();
		tokio::spawn(async move {
			loop {
				let config = state.config().await;
				sleep(Duration::from_secs(config.queue_sleep_duration.unwrap_or(1))).await;

				if queue.is_paused() {
					continue;
				}

				if let Err(err) = queue.send_one(&config.tweaker).await {
					tracing::error!("Error sending response: {err}");
				}
			}
		});
	}
}

impl Clone for ResponseQueue {
	fn clone(&self) -> Self {
		Self {
			paused: self.paused.clone(),
			queue: self.queue.clone()
		}
	}
}


pub(crate) async fn join_room(room: Room) {
	if room.state() == RoomState::Joined {
		return;
	}

	tokio::spawn(async move {
		let rid = room.room_id();
		tracing::info!("Joining room {rid}");
		let mut delay = 2;

		while let Err(err) = room.join().await {
			tracing::error!(
				"Failed to join room {rid}: {err:?}, retrying in {delay}s"
			);

			sleep(Duration::from_secs(delay)).await;
			delay *= 2;
		}
	});
}

pub(crate) async fn join_or_knock(state: &State, room_id: OwnedRoomOrAliasId)
-> crate::Result<()> {
	let config = state.config().await;
	let servers = config.join_by_servers();
	let preview = state.client.get_room_preview(&room_id, servers.clone()).await?;

	if preview.join_rule == Some(JoinRuleSummary::Knock) {
		tracing::info!("knocking {room_id}");
		state.client.knock(
			room_id, config.knock_reason.clone(), servers
		).await?;
	}
	else {
		tracing::info!("joining {room_id}");
		state.client.join_room_by_id_or_alias(&room_id, &servers).await?;
	}

	Ok(())
}

async fn on_invite(event: StrippedRoomMemberEvent, client: Client, room: Room) {
	if event.state_key != client.user_id().unwrap() {
		return;
	}
	join_room(room).await;
}

async fn log_ban(event: OriginalSyncRoomMemberEvent, client: Client, room: Room) {
	if event.state_key != client.user_id().unwrap() {
		return;
	}

	if event.content.membership == MembershipState::Ban {
		let reason = event.content.reason.unwrap_or("<none>".to_string());
		tracing::error!("Banned from {} for `{}`", room.room_id(), reason);
	}
}

async fn on_message(
	message: OriginalSyncRoomMessageEvent, room: Room, Ctx(state): Ctx<State>
) -> crate::Result<()> {
	let config = state.config().await;
	let client_id = state.client.user_id().unwrap();

	if message.sender == client_id && !config.respond_to_self {
		return Ok(());
	}

	let props = MessageProps::for_message(
		&room, &message, client_id.to_string() ,&config.tweaker.keywords
	).await;

	let age = props.now - props.timestamp;
	if age > TimeDelta::seconds(config.message_max_age) {
		tracing::debug!("[{}] message is too old ({age}s)", room.room_id());
		return Ok(());
	}

	match state.console.run_message(state.clone(), room.clone(), message.clone()).await? {
		MessageCommandResult::NotHandled => {},
		MessageCommandResult::Handled => { return Ok(()); },
		MessageCommandResult::HandledWithResponse(res) => {
			return res.send(&room).await;
		}
	};

	log_message(&config, &room, &message)?;

	let body = message.content.body();
	if body.is_empty() {
		return Ok(());
	}

	find_and_join_rooms(&state, body).await?;

	let response = state.tweaker.tweak_on_message(
		state.clone(), room.clone(), message, props
	).await?;

	if matches!(response, Response::None) {
		return Ok(());
	}

	state.queue.add(response, room).await;

	Ok(())
}

async fn copy_reactions(
	reaction: OriginalSyncReactionEvent, room: Room, Ctx(state): Ctx<State>
) -> crate::Result<()> {
	if reaction.sender == state.client.user_id().unwrap() {
		return Ok(());
	}

	let config = state.config().await.tweaker;
	let mode = state.tweaker.tweak_mode(&config, room.room_id().as_str()).await;
	if mode == TweakMode::Disabled {
		return Ok(());
	}

	let rand = rng().random_range(0.0f64..1.0);
	if rand < state.config().await.tweaker.copy_reactions {
		state.queue.add(Response::Reaction(reaction.content), room).await;
	}
	Ok(())
}

pub(crate) async fn start(state: State) -> crate::Result<()> {
	state.client.add_event_handler_context(state.clone());
	state.client.add_event_handler(on_message);
	state.client.add_event_handler(log_ban);

	let config = state.config().await;
	if config.accept_invites {
		state.client.add_event_handler(on_invite);
	}

	if config.tweaker.copy_reactions > 0.0 {
		state.client.add_event_handler(copy_reactions);
	}

	let mut ss = SyncSettings::default();
	if let Some(timeout) = config.sync_timeout {
		ss = ss.timeout(Duration::from_secs(timeout));
	}

	tracing::info!("Starting sync");
	loop {
		let res = state.client.sync(ss.clone()).await;
		if let Err(err) = res {
			tracing::error!("Failed to sync: {err:?}");
		}
	}
}
