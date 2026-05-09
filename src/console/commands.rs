#![allow(clippy::get_first)]

use super::{Command, Console, CommandContext, TrustLevel};
use crate::{State, bot::{MessageProps, join_or_knock}, tweak::TweakMode};
use std::{cmp::max, str::FromStr};
use async_trait::async_trait;
use matrix_sdk::ruma::{OwnedRoomOrAliasId, RoomId};
use chrono::{Utc, Local, TimeDelta};

const COL_SEP_W: usize = 2;

struct InfoCommand;
#[async_trait]
impl Command for InfoCommand {
	fn name(&self) -> &'static str {
		"info"
	}

	fn trust_level(&self) -> TrustLevel {
		TrustLevel::Untrusted
	}

	async fn run(
		&self, _state: State, _args: Vec<String>, _context: CommandContext
	) -> crate::Result<Option<String>> {
		let name = env!("CARGO_PKG_NAME");
		let version = env!("CARGO_PKG_VERSION");
		let start = crate::START_TIME.get().unwrap();
		let uptime = Utc::now() - start;

		let out = format!(
			"{name} v{version}\nUptime: {:02}:{:02}:{:02}.{:03}",
			uptime.num_hours(), uptime.num_minutes() % 60,
			uptime.num_seconds() % 3600, uptime.subsec_millis()
		);
		Ok(Some(out))
	}
}

struct CommandsCommand;
#[async_trait]
impl Command for CommandsCommand {
	fn name(&self) -> &'static str {
		"help"
	}

	fn trust_level(&self) -> TrustLevel {
		TrustLevel::Untrusted
	}

	async fn run(
		&self, state: State, _args: Vec<String>, context: CommandContext
	) -> crate::Result<Option<String>> {
		let mut commands = state.console.command_list().await.into_iter()
			.map(|(name, tl, pl)| (name, format!("{tl:?}"), format!("{pl}")))
			.collect::<Vec<(String, String, String)>>();
		commands.sort();

		let (max_name, max_tl) = commands.iter()
			.fold((0, 0), |(mn, mtl), (name, tl, _)| (max(mn, name.len()), max(mtl, tl.len())));

		let config = state.config().await.console;
		let mut out = format!("Your trust level: {:?}\n\n", context.trust_level(&config));

		for (name, tl, pl) in commands.iter() {
			out.push_str(
				&format!(
					"{name}{}{tl}{}{pl}\n",
					" ".repeat(max_name - name.len() + COL_SEP_W),
					" ".repeat(max_tl - tl.len() + COL_SEP_W),
				)
			);
		}

		Ok(Some(out))
	}
}

struct ReloadConfigCommand;
#[async_trait]
impl Command for ReloadConfigCommand {
	fn name(&self) -> &'static str {
		"rlconfig"
	}

	fn trust_level(&self) -> TrustLevel {
		TrustLevel::Trusted
	}

	async fn run(
		&self, state: State, _args: Vec<String>, _context: CommandContext
	) -> crate::Result<Option<String>> {
		state.reload_config().await?;
		Ok(None)
	}
}

struct TweakCommand;
#[async_trait]
impl Command for TweakCommand {
	fn name(&self) -> &'static str {
		"tweak"
	}

	fn trust_level(&self) -> TrustLevel {
		TrustLevel::Untrusted
	}

	async fn run(
		&self, state: State, args: Vec<String>, _context: CommandContext
	) -> crate::Result<Option<String>> {
		let length = args.get(0).map(|val| val.parse::<usize>().unwrap_or(256)).unwrap_or(256);
		let start = args.get(1).cloned();
		let end = args.get(2).cloned();

		let tweak = state.tweaker.tweak_filtered(&state.config().await.tweaker, length, start, end).await?;
		Ok(Some(tweak))
	}
}

struct TweakUnfilteredCommand;
#[async_trait]
impl Command for TweakUnfilteredCommand {
	fn name(&self) -> &'static str {
		"tweakuf"
	}

	fn trust_level(&self) -> TrustLevel {
		TrustLevel::Trusted
	}

	async fn run(
		&self, state: State, args: Vec<String>, _context: CommandContext
	) -> crate::Result<Option<String>> {
		let length = args.get(0).map(|val| val.parse::<usize>().unwrap_or(256)).unwrap_or(256);
		let start = args.get(1).cloned();
		let end = args.get(2).cloned();

		let tweak = state.tweaker.tweak(length, start, end).await?;
		Ok(Some(tweak))
	}
}

struct MarkovInfoCommand;
#[async_trait]
impl Command for MarkovInfoCommand {
	fn name(&self) -> &'static str {
		"markov"
	}

	fn trust_level(&self) -> TrustLevel {
		TrustLevel::Untrusted
	}

	async fn run(
		&self, state: State, _args: Vec<String>, _context: CommandContext
	) -> crate::Result<Option<String>> {
		let (seed_len, dict_len) = state.tweaker.markov_info().await;
		let out = format!("Seed length: {seed_len}\nDictionary length: {dict_len}");
		Ok(Some(out))
	}
}

struct LoadCommand;
#[async_trait]
impl Command for LoadCommand {
	fn name(&self) -> &'static str {
		"load"
	}

	fn trust_level(&self) -> TrustLevel {
		TrustLevel::Trusted
	}

	async fn run(
		&self, state: State, args: Vec<String>, _context: CommandContext
	) -> crate::Result<Option<String>> {
		let name = args.get(0).map(|n| n.as_str());
		state.tweaker.load_self(&state.config().await.tweaker, name).await?;
		Ok(None)
	}
}

struct CheckpointCommand;
#[async_trait]
impl Command for CheckpointCommand {
	fn name(&self) -> &'static str {
		"checkpoint"
	}

	fn trust_level(&self) -> TrustLevel {
		TrustLevel::Trusted
	}

	async fn run(
		&self, state: State, _args: Vec<String>, _context: CommandContext
	) -> crate::Result<Option<String>> {
		state.tweaker.checkpoint(&state.config().await.tweaker).await?;
		Ok(None)
	}
}

struct ListRoomsCommand;
#[async_trait]
impl Command for ListRoomsCommand {
	fn name(&self) -> &'static str {
		"rooms"
	}

	fn trust_level(&self) -> TrustLevel {
		TrustLevel::Untrusted
	}

	async fn run(
		&self, state: State, _args: Vec<String>, _context: CommandContext
	) -> crate::Result<Option<String>> {
		let rooms: Vec<(String, String)> = state.client.rooms().iter()
			.map(|room| (room.room_id().to_string(), room.name().unwrap_or(String::new())))
			.collect();
		let max = rooms.iter().fold(0, |a, (id, _)| max(a, id.len()));
		let out = rooms.iter()
			.map(|(id, name)| format!("{id}{}{name}", " ".repeat((max - id.len()) + COL_SEP_W)))
			.collect::<Vec<String>>()
			.join("\n");
		Ok(Some(out))
	}
}

struct JoinRoomCommand;
#[async_trait]
impl Command for JoinRoomCommand {
	fn name(&self) -> &'static str {
		"join"
	}

	async fn run(
		&self, state: State, args: Vec<String>, _context: CommandContext
	) -> crate::Result<Option<String>> {
		let room_id = args.get(0).ok_or("Room id/alias required")?;
		let room_id = OwnedRoomOrAliasId::try_from(room_id.as_str())?;
		join_or_knock(&state, room_id).await?;
		Ok(None)
	}
}

struct LeaveRoomCommand;
#[async_trait]
impl Command for LeaveRoomCommand {
	fn name(&self) -> &'static str {
		"leave"
	}

	async fn run(
		&self, state: State, args: Vec<String>, _context: CommandContext
	) -> crate::Result<Option<String>> {
		let room_id = args.get(0).ok_or("Room id required")?;
		let room_id = <&RoomId>::try_from(room_id.as_str())?;
		let room = state.client.get_room(room_id)
			.ok_or("Room does not exist")?;

		tracing::info!("Leaving {room_id}");
		room.leave().await?;
		Ok(None)
	}
}

struct RoomInfoCommand;
#[async_trait]
impl Command for RoomInfoCommand {
	fn name(&self) -> &'static str {
		"roominfo"
	}

	fn trust_level(&self) -> TrustLevel {
		TrustLevel::Untrusted
	}

	async fn run(
		&self, state: State, args: Vec<String>, context: CommandContext
	) -> crate::Result<Option<String>> {
		let room_id = match (args.get(0), context) {
			(Some(id), _) => id.clone(),
			(None, CommandContext::Chat{room, message: _}) => room.room_id().to_string(),
			(None, CommandContext::Console) =>
				{ return Err("Room id required in non-chat context".into()); }
		};

		let config = state.config().await;
		let mode = state.tweaker.tweak_mode(&config.tweaker, &room_id).await;
		let last_msg = state.tweaker.last_message(&room_id).await;
		let delta = Utc::now() - last_msg;
		let last_msg = last_msg.with_timezone(&Local);

		let mut cooldowns: Vec<String> = vec![];
		if delta < TimeDelta::seconds(config.tweaker.cooldown) {
			cooldowns.push("message".to_string());
		}
		if delta < TimeDelta::seconds(config.tweaker.cooldown_react) {
			cooldowns.push("react".to_string());
		}

		let mut out = format!("Response mode: {mode:?}\nLast message: {last_msg}");
		if !cooldowns.is_empty() {
			out.push_str(" (cooldowns: ");
			out.push_str(&cooldowns.join(", "));
			out.push(')');
		}

		Ok(Some(out))
	}
}

struct RoomModeCommand;
#[async_trait]
impl Command for RoomModeCommand {
	fn name(&self) -> &'static str {
		"mode"
	}

	fn power_level(&self) -> i32 {
		50
	}

	async fn run(
		&self, state: State, args: Vec<String>, context: CommandContext
	) -> crate::Result<Option<String>> {
		let mode = args.get(0).ok_or("Mode must be specified")?;
		let mode = TweakMode::from_str(mode)?;

		let room_id = match context {
			CommandContext::Chat{room, message: _} => room.room_id().to_string(),
			CommandContext::Console => args.get(1).cloned()
				.ok_or("Room must be specified in non-chat context")?
		};

		state.tweaker.set_mode(room_id, mode).await;
		Ok(None)
	}
}

struct MessageInfoCommand;
#[async_trait]
impl Command for MessageInfoCommand {
	fn name(&self) -> &'static str {
		"msginfo"
	}

	fn trust_level(&self) -> TrustLevel {
		TrustLevel::Untrusted
	}

	async fn run(
		&self, state: State, _args: Vec<String>, context: CommandContext
	) -> crate::Result<Option<String>> {
		let (room, message) = match context {
			CommandContext::Chat{room, message} => (room, message),
			CommandContext::Console => {
				return Err("Must be run from chat context".into());
			}
		};

		let props = MessageProps::for_message(
			&room, &message,
			state.client.user_id().unwrap().to_string(),
			&state.config().await.tweaker.keywords
		).await;

		let keyword = props.keyword
			.map(|k| k.keyword())
			.unwrap_or("<none>".to_string());
		let reply = props.reply.then_some("yes").unwrap_or("no");

		let out = format!("Keyword: {keyword}\nIs reply: {reply}");
		Ok(Some(out))
	}
}

struct QueueCommand;
#[async_trait]
impl Command for QueueCommand {
	fn name(&self) -> &'static str {
		"queue"
	}

	fn trust_level(&self) -> TrustLevel {
		TrustLevel::Trusted
	}

	async fn run(
		&self, state: State, args: Vec<String>, _context: CommandContext
	) -> crate::Result<Option<String>> {
		match args.get(0).map(|a| a.as_str()) {
			Some("pause") => {
				state.queue.set_paused(true);
				Ok(None)
			},
			Some("unpause") => {
				state.queue.set_paused(false);
				Ok(None)
			},
			Some("purge") => {
				state.queue.purge().await;
				Ok(None)
			}
			_ => {
				let out = format!(
					"There are {} responses in the queue\nPaused: {}",
					state.queue.len().await,
					state.queue.is_paused().then_some("yes").unwrap_or("no")
				);
				Ok(Some(out))
			}
		}
	}
}

struct SleepCommand;
#[async_trait]
impl Command for SleepCommand {
	fn name(&self) -> &'static str {
		"sleepctl"
	}

	fn trust_level(&self) -> TrustLevel {
		TrustLevel::Trusted
	}

	async fn run(
		&self, state: State, args: Vec<String>, _context: CommandContext
	) -> crate::Result<Option<String>> {
		match args.get(0).map(|a| a.as_str()) {
			Some("sleep") => {
				state.tweaker.set_sleeping(&state.queue, true, false).await;
				Ok(None)
			},
			Some("wake") => {
				state.tweaker.set_sleeping(&state.queue, false, false).await;
				Ok(None)
			},
			Some("nyquil") => {
				state.tweaker.set_sleeping(&state.queue, true, true).await;
				Ok(Some("So eepy...".to_string()))
			},
			Some("redbull") => {
				state.tweaker.set_sleeping(&state.queue, false, true).await;
				Ok(Some("HOLY SHIT I COULD TWEAK ALL NIGHT!!!".to_string()))
			},
			_ => {
				let (sleeping, locked) = state.tweaker.is_sleeping().await;
				let out = format!(
					"Currently {} (locked: {})",
					sleeping.then_some("sleeping").unwrap_or("awake"),
					locked.then_some("yes").unwrap_or("no")
				);
				Ok(Some(out))
			}
		}
	}
}

pub(crate) async fn register_commands(commands: &Console) {
	commands.register(InfoCommand).await;
	commands.register(CommandsCommand).await;
	commands.register(ReloadConfigCommand).await;
	commands.register(TweakCommand).await;
	commands.register(TweakUnfilteredCommand).await;
	commands.register(MarkovInfoCommand).await;
	commands.register(LoadCommand).await;
	commands.register(CheckpointCommand).await;
	commands.register(ListRoomsCommand).await;
	commands.register(JoinRoomCommand).await;
	commands.register(LeaveRoomCommand).await;
	commands.register(RoomInfoCommand).await;
	commands.register(RoomModeCommand).await;
	commands.register(MessageInfoCommand).await;
	commands.register(QueueCommand).await;
	commands.register(SleepCommand).await;
}
