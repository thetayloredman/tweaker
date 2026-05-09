use bot::{Account, ResponseQueue};
use crawler::CrawlerConfig;
use tweak::{Tweaker, TweakerConfig};
use console::{Console, ConsoleConfig};

use std::{fs::File, io::Read, error::Error, path::PathBuf, sync::Arc};
use serde::Deserialize;
use matrix_sdk::{Client, ruma::OwnedServerName};
use tracing_subscriber::{fmt::time::LocalTime, filter::EnvFilter};
use tokio::{runtime::Builder, sync::{OnceCell, RwLock}};
use chrono::{DateTime, Utc};

pub mod tweak;
pub mod bot;
pub mod crawler;
pub mod console;

const DEFAULT_CONFIG_NAME: &str = "tweaker.toml";
const CONFIG_ENV_VAR: &str = "CONFIG";

static START_TIME: OnceCell<DateTime<Utc>> = OnceCell::const_new();

pub(crate) type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Deserialize, Clone)]
struct Config {
	join_by_servers: Vec<String>,
	sync_timeout: Option<u64>,
	request_timeout: Option<u64>,
	connect_timeout: Option<u64>,
	user_agent: Option<String>,
	accept_invites: bool,
	knock_reason: Option<String>,
	pub(crate) respond_to_self: bool,
	message_max_age: i64,
	queue_sleep_duration: Option<u64>,
	account: Account,
	crawler: CrawlerConfig,
	tweaker: TweakerConfig,
	console: ConsoleConfig
}

impl Config {
	fn load(path: &PathBuf) -> crate::Result<Self> {
		tracing::info!("Loading configuration from {}", path.to_string_lossy());
		let mut file = File::open(path)?;
		let mut buf = Vec::<u8>::new();
		file.read_to_end(&mut buf)?;
		Ok(toml::from_slice(&buf)?)
	}

	fn load_from_env() -> crate::Result<Self> {
		let path = match std::env::var(CONFIG_ENV_VAR) {
			Ok(path) => PathBuf::from(path),
			Err(_) => {
				let mut dir = std::env::current_dir()?;
				dir.push(DEFAULT_CONFIG_NAME);
				dir
			}
		};
		Self::load(&path)
	}

	pub(crate) fn join_by_servers(&self) -> Vec<OwnedServerName> {
		self.join_by_servers.iter()
			.map(|name| OwnedServerName::try_from(name.clone()))
			.filter_map(|res| res.ok())
			.collect::<Vec<OwnedServerName>>()
	}
}

#[derive(Clone)]
struct State {
	config: Arc<RwLock<Config>>,
	client: Client,
	tweaker: Tweaker,
	console: Console,
	queue: ResponseQueue
}

impl State {
	fn new(config: Config, client: Client, tweaker: Tweaker, console: Console) -> Self {
		Self {
			config: Arc::new(RwLock::new(config)),
			client,
			tweaker,
			console,
			queue: ResponseQueue::new()
		}
	}

	async fn config(&self) -> Config {
		let config = self.config.read().await;
		config.clone()
	}

	async fn reload_config(&self) -> crate::Result<()> {
		let mut config = self.config.write().await;
		*config = Config::load_from_env()?;
		Ok(())
	}
}


#[tokio::main]
async fn main() -> crate::Result<()> {
	tracing_subscriber::fmt()
		.with_env_filter(EnvFilter::from_default_env())
		.with_timer(LocalTime::rfc_3339())
		.init();

	START_TIME.set(Utc::now()).unwrap();

	let config = Config::load_from_env()?;
	let client = config.account.login(&config).await?;
	let tweaker = Tweaker::load(&config.tweaker, None, None)?;

	let ccc = config.clone();
	let cct = tweaker.clone();
	ctrlc::set_handler(move || {
		Builder::new_current_thread().enable_all().build().unwrap()
			.block_on(cct.checkpoint(&ccc.tweaker))
			.expect("Failed to save checkpoint");

		std::process::exit(0);
	})?;

	let console = Console::new();
	crate::console::commands::register_commands(&console).await;

	let state = State::new(config, client, tweaker, console);
	state.console.prompt_loop(state.clone()).await;
	state.queue.send_loop(state.clone()).await;
	state.tweaker.sleep_loop(&state).await;
	bot::start(state).await?;
	Ok(())
}
