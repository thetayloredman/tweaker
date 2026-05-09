use crate::{State, bot::Response};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;
use async_trait::async_trait;
use matrix_sdk::{
	Room,
	ruma::{
		Int,
		events::room::{
			power_levels::UserPowerLevel,
			message::{
				MessageType, OriginalSyncRoomMessageEvent
			}
		}
	}
};
use serde::Deserialize;

pub(crate) mod commands;

const CHECKMARK: &str = "✅";

fn prompt() -> Result<String, std::io::Error> {
	let mut line = String::new();
	std::io::stdin().read_line(&mut line)?;
	Ok(line.strip_suffix('\n').unwrap().to_string())
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum TrustLevel {
	Untrusted,
	Trusted,
	FullTrusted,
	Operator
}

impl TrustLevel {
	fn bypass_pl_checks(&self) -> bool {
		self >= &Self::FullTrusted
	}
}

impl Default for TrustLevel {
	fn default() -> Self {
		Self::Untrusted
	}
}

#[derive(Deserialize, Clone)]
pub(crate) struct ConsoleConfig {
	enable_chat: bool,
	prefix: String,
	trust_levels: HashMap<String, TrustLevel>
}

impl ConsoleConfig {
	fn trust_level(&self, user_id: &str) -> TrustLevel {
		self.trust_levels.get(user_id).cloned()
		.unwrap_or(
			self.trust_levels.get("default").cloned()
				.unwrap_or_default()
		)
	}
}

pub(crate) enum CommandContext {
	Console,
	Chat {
		room: Room,
		message: OriginalSyncRoomMessageEvent
	}
}

impl CommandContext {
	pub(crate) fn trust_level(&self, config: &ConsoleConfig) -> TrustLevel {
		match self {
			Self::Console => TrustLevel::Operator,
			Self::Chat {room: _, message} => config.trust_level(message.sender.as_str())
		}
	}

	pub(crate) async fn validate_power_level(&self, pl: i32)
	-> crate::Result<bool> {
		match self {
			Self::Console => Ok(true),
			Self::Chat {room, message} => {
				let upl = room.get_user_power_level(&message.sender).await?;
				Ok(
					match upl {
						UserPowerLevel::Infinite => true,
						UserPowerLevel::Int(int) => int >= Int::from(pl),
						_ => false
					}
				)
			}
		}
	}
}

#[async_trait]
pub(crate) trait Command: Send + Sync + 'static {
	fn name(&self) -> &'static str;

	fn trust_level(&self) -> TrustLevel {
		TrustLevel::Operator
	}

	fn power_level(&self) -> i32 {
		0
	}

	async fn run(
		&self, state: State, args: Vec<String>, context: CommandContext
	) -> crate::Result<Option<String>>;
}

pub(crate) enum MessageCommandResult {
	NotHandled,
	Handled,
	HandledWithResponse(Response)
}

struct ConsoleInner {
	commands: HashMap<String, Box<dyn Command>>
}

pub(crate) struct Console {
	inner: Arc<RwLock<ConsoleInner>>
}

impl Console {
	pub(crate) fn new() -> Self {
		Self {
			inner: Arc::new(RwLock::new(
				ConsoleInner {
					commands: HashMap::new()
				}
			))
		}
	}

	pub(crate) async fn len(&self) -> usize {
		let inner = self.inner.read().await;
		inner.commands.len()
	}

	pub(crate) async fn command_list(&self) -> Vec<(String, TrustLevel, i32)> {
		let inner = self.inner.read().await;
		inner.commands.iter()
			.map(|(name, cmd)| (name.clone(), cmd.trust_level(), cmd.power_level()))
			.collect::<Vec<(String, TrustLevel, i32)>>()
	}

	pub(crate) async fn register(&self, func: impl Command) {
		let mut inner = self.inner.write().await;
		inner.commands.insert(func.name().to_string(), Box::new(func));
	}

	pub(crate) async fn run(
		&self, state: State, command: &str, context: CommandContext
	) -> crate::Result<Option<String>> {
		let inner = self.inner.read().await;
		let (command, args) = cmdparse::parse(command);
		match inner.commands.get(&command) {
			Some(command) => {
				let trust_level = context.trust_level(&state.config().await.console);
				if trust_level < command.trust_level() {
					return Err("Insufficient trust level".into());
				}

				if !trust_level.bypass_pl_checks()
				&& !context.validate_power_level(command.power_level()).await? {
					return Err("Insufficient power level".into());
				}

				command.run(state, args, context).await
			},
			None => Err("Command does not exist".into())
		}
	}

	pub(crate) async fn run_prompt(&self, state: State) {
		match prompt() {
			Ok(command) => {
				match self.run(state.clone(), &command, CommandContext::Console).await {
					Ok(None) => tracing::info!("Success"),
					Ok(Some(res)) => tracing::info!("Result: {res}"),
					Err(err) => tracing::error!("Error in command: {err}"),
				};
			},
			Err(err) => tracing::error!("Error reading stdin: {err}")
		};
	}

	pub(crate) async fn prompt_loop(&self, state: State) {
		let con = self.clone();
		tokio::spawn(async move {
			loop {
				con.run_prompt(state.clone()).await;
			}
		});
	}

	pub(crate) async fn run_message(
		&self, state: State, room: Room, message: OriginalSyncRoomMessageEvent
	) -> crate::Result<MessageCommandResult> {
		let config = state.config().await.console;
		if !config.enable_chat {
			return Ok(MessageCommandResult::NotHandled);
		}

		let body = message.content.body();
		if !matches!(message.content.msgtype, MessageType::Text(_)) || body.is_empty() {
			return Ok(MessageCommandResult::NotHandled);
		}

		if let Some(command) = body.strip_prefix(&config.prefix) {
			let room_id = room.room_id();
			let user_id = message.sender.clone();
			tracing::info!("[{room_id}] {user_id} sent command `{command}`");

			let context = CommandContext::Chat {
				room: room.clone(),
				message: message.clone()
			};

			let res = match self.run(state, command, context).await {
				Ok(None) => {
					tracing::debug!("success");
					Response::reaction(message.event_id, CHECKMARK.to_string())
				},
				Ok(Some(out)) => {
					tracing::info!("success: {out}");
					Response::message(&out, Some(&message))
				},
				Err(err) => {
					tracing::error!("error: {err}");
					let em = format!("error executing command: {err}");
					Response::message(&em, Some(&message))
				}
			};
			return Ok(MessageCommandResult::HandledWithResponse(res));
		}

		Ok(MessageCommandResult::NotHandled)
	}
}

impl Clone for Console {
	fn clone(&self) -> Self {
		Self {
			inner: self.inner.clone()
		}
	}
}
