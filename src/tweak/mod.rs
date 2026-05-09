use crate::{State, bot::{MessageProps, Response, ResponseQueue}};
use tweakov::{WeightedMarkovGenerator, serialize::Serialize};
use std::{
	fs::File, collections::HashMap,
	path::PathBuf, sync::Arc,
	io::{BufReader, BufWriter, Read, Write}
};
use tokio::{sync::RwLock, time::Duration};
use serde::Deserialize;
use chrono::{DateTime, Local, TimeDelta, Timelike, Utc, NaiveTime};
use rand::{RngExt, rng, seq::{IndexedRandom, IteratorRandom}};
use matrix_sdk::{
	Room,
	ruma::events::room::message::OriginalSyncRoomMessageEvent
};
use strum::EnumString;

mod filters;

type MarkovGenerator = WeightedMarkovGenerator<Vec<u8>>;

const CURRENT_GEN_FILE: &str = "current.tkv";
const SLEEP_LOOP_DURATION: u64 = 60;

#[allow(clippy::enum_variant_names)]
#[derive(Deserialize, Clone)]
#[serde(untagged)]
pub(crate) enum FilterData {
	NoArgs(String),
	InlineArgs(Vec<String>),
	ArrayArgs(String, Vec<String>)
}

impl FilterData {
	fn name(&self) -> String {
		match self {
			Self::NoArgs(name) => name.clone(),
			Self::InlineArgs(args) => args.first().cloned().unwrap_or_default(),
			Self::ArrayArgs(name, _) => name.clone()
		}
	}

	fn args(&self) -> Vec<String> {
		match self {
			Self::NoArgs(_) => vec![],
			Self::InlineArgs(args) => args.iter().skip(1).cloned().collect(),
			Self::ArrayArgs(_, args) => args.to_vec()
		}
	}
}

pub(crate) trait Filter: Send + Sync + 'static {
	fn name(&self) -> &'static str;
	fn apply(&self, string: String, args: Vec<String>) -> crate::Result<String>;
}

pub(crate) struct Filters {
	filters: HashMap<String, Box<dyn Filter>>
}

impl Filters {
	pub(crate) fn new() -> Self {
		Self {
			filters: HashMap::new()
		}
	}

	pub(crate) fn register(&mut self, filter: impl Filter) {
		self.filters.insert(filter.name().to_string(), Box::new(filter));
	}

	pub(crate) fn apply(&self, mut string: String, filters: Vec<FilterData>)
	-> crate::Result<String> {
		for fd in filters {
			let name = fd.name();
			let filter = self.filters.get(&name)
				.ok_or(format!("Filter {name} does not exist"))?;
			string = filter.apply(string, fd.args())?;
		}
		Ok(string)
	}
}

impl Default for Filters {
	fn default() -> Self {
		let mut fs = Self::new();
		filters::register_filters(&mut fs);
		fs
	}
}

#[derive(Deserialize, Clone, PartialEq, Debug, EnumString)]
pub(crate) enum TweakMode {
	Disabled,
	Reactions,
	KeywordsOnly,
	KeywordsAndReplies,
	Normal
}

#[derive(Deserialize, Clone)]
#[serde(untagged)]
pub(crate) enum Keyword {
	StartOnNotSpecified(String),
	StartOnSpecified(String, bool)
}

impl Keyword {
	pub(crate) fn keyword(&self) -> String {
		match self {
			Self::StartOnNotSpecified(keyword) => keyword.clone(),
			Self::StartOnSpecified(keyword, _) => keyword.clone()
		}
	}

	pub(crate) fn start_on(&self) -> bool {
		match self {
			Self::StartOnNotSpecified(_) => true,
			Self::StartOnSpecified(_, start_on) => *start_on
		}
	}
}

#[derive(Deserialize, Clone)]
pub(crate) struct TweakerConfig {
	gen_directory: PathBuf,
	seed_length: usize,
	message_seed_factor: f64,
	do_not_tweak: Option<bool>,
	prob_tweak: f64,
	prob_tweak_keyword: f64,
	prob_tweak_reply: f64,
	prob_react: f64,
	prob_reply: f64,
	pub(crate) cooldown: i64,
	pub(crate) cooldown_react: i64,
	length_min: usize,
	length_max: usize,
	ignore_words: Vec<String>,
	endings: Vec<String>,
	pub(crate) keywords: Vec<Keyword>,
	tweak_mode: HashMap<String, TweakMode>,
	pub(crate) filters: Vec<FilterData>,
	remove_start_text: bool,
	reactions: HashMap<String, f64>,
	pub(crate) delay_min: u64,
	pub(crate) delay_max: u64,
	pub(crate) copy_reactions: f64,
	pub(crate) typing_time: f64,
	pub(crate) sleep_times: Vec<NaiveTime>,
	pub(crate) wake_times: Vec<NaiveTime>,
}

impl TweakerConfig {
	pub(crate) fn tweak_mode(&self, room_id: &str) -> TweakMode {
		self.tweak_mode.get(room_id).cloned().unwrap_or(
			self.tweak_mode.get("default").cloned()
				.unwrap_or(TweakMode::KeywordsAndReplies)
		)
	}
}

#[derive(PartialEq, Debug)]
pub(crate) enum TweakAction {
	None,
	Message,
	Reply,
	React
}

struct TweakerInner {
	markov: MarkovGenerator,
	filters: Filters,
	last_message: HashMap<String, DateTime<Utc>>,
	mode_override: HashMap<String, TweakMode>,
	sleeping: bool,
	sleeping_locked: bool
}

pub(crate) struct Tweaker {
	inner: Arc<RwLock<TweakerInner>>
}

impl Tweaker {
	pub(crate) fn new(markov: MarkovGenerator, filters: Filters) -> Self {
		Self {
			inner: Arc::new(RwLock::new(
				TweakerInner {
					markov,
					filters,
					last_message: HashMap::new(),
					mode_override: HashMap::new(),
					sleeping: false,
					sleeping_locked: false
				}
			))
		}
	}

	fn load_markov(config: &TweakerConfig, filename: Option<&str>)
	-> crate::Result<MarkovGenerator> {
		let mut path = config.gen_directory.clone();
		if !path.is_dir() {
			return Err("config.tweaker.gen_directory is not a directory".into());
		}

		path.push(filename.unwrap_or(CURRENT_GEN_FILE));
		tracing::info!("Loading markov generator from {}", path.to_string_lossy());
		let mg = match File::open(path) {
			Ok(file) => {
				let mut br = BufReader::new(file);
				MarkovGenerator::deserialize(&mut br)?
			},
			Err(_) => MarkovGenerator::new(config.seed_length)
		};
		Ok(mg)
	}

	pub(crate) fn load(config: &TweakerConfig, filename: Option<&str>, filters: Option<Filters>)
	-> crate::Result<Self> {
		let mg = Self::load_markov(config, filename)?;
		Ok(Self::new(mg, filters.unwrap_or_default()))
	}

	pub(crate) async fn load_self(&self, config: &TweakerConfig, filename: Option<&str>)
	-> crate::Result<()> {
		let mg = Self::load_markov(config, filename)?;
		let mut inner = self.inner.write().await;
		inner.markov = mg;
		Ok(())
	}

	pub(crate) async fn markov_info(&self) -> (usize, usize) {
		let inner = self.inner.read().await;
		(inner.markov.seed_length(), inner.markov.seed_dictionary_length())
	}

	pub(crate) async fn save(&self, config: &TweakerConfig, filename: &str)
	-> crate::Result<PathBuf> {
		let mut path = config.gen_directory.clone();
		path.push(filename);
		tracing::info!("Saving markov generator to {}", path.to_string_lossy());
		let file = File::create(&path)?;
		let mut bw = BufWriter::new(file);
		let inner = self.inner.read().await;
		inner.markov.serialize(&mut bw)?;
		Ok(path)
	}

	pub(crate) async fn checkpoint(&self, config: &TweakerConfig)
	-> crate::Result<()> {
		let mut name = "checkpoint_".to_string();
		name.push_str(&Local::now().to_rfc3339());
		name.push_str(".tkv");
		let path = self.save(config, &name).await?;

		let mut file = File::open(path)?;
		let mut buf = Vec::<u8>::new();
		file.read_to_end(&mut buf)?;

		let mut path = config.gen_directory.clone();
		path.push(CURRENT_GEN_FILE);
		let mut current = File::create(path)?;
		current.write_all(&buf)?;
		Ok(())
	}

	pub(crate) async fn ingest(&self, config: &TweakerConfig, text: Vec<u8>) {
		let mut inner = self.inner.write().await;
		inner.markov.seed_text(text, config.message_seed_factor);
	}

	pub(crate) async fn last_message(&self, room_id: &str) -> DateTime<Utc> {
		let inner = self.inner.read().await;
		inner.last_message.get(room_id).cloned()
			.unwrap_or(DateTime::UNIX_EPOCH)
	}

	pub(crate) async fn tweak_mode(&self, config: &TweakerConfig, room_id: &str) -> TweakMode {
		let inner = self.inner.read().await;
		inner.mode_override.get(room_id).cloned()
			.unwrap_or(config.tweak_mode(room_id))
	}

	pub(crate) async fn set_mode(&self, room_id: String, mode: TweakMode) {
		let mut inner = self.inner.write().await;
		inner.mode_override.insert(room_id, mode);
	}

	pub(crate) async fn is_sleeping(&self) -> (bool, bool) {
		let inner = self.inner.read().await;
		(inner.sleeping, inner.sleeping_locked)
	}

	pub(crate) async fn set_sleeping(&self, queue: &ResponseQueue, sleep: bool, lock: bool) {
		tracing::warn!("Sleep mode changed to {sleep} (locked: {lock})");
		let mut inner = self.inner.write().await;
		inner.sleeping = sleep;
		inner.sleeping_locked = lock;
		queue.set_paused(sleep);
	}

	pub(crate) async fn sleep_loop(&self, state: &State) {
		let tweaker = self.clone();
		let state = state.clone();
		tokio::spawn(async move {
			loop {
				tokio::time::sleep(Duration::from_secs(SLEEP_LOOP_DURATION)).await;
				let (sleeping, locked) = tweaker.is_sleeping().await;

				if locked {
					continue;
				}

				let config = state.config().await.tweaker;
				let now = Local::now().time();
				if sleeping {
					for time in &config.wake_times {
						if time.hour() == now.hour() && time.minute() == now.minute() {
							tweaker.set_sleeping(&state.queue, false, false).await;
						}
					}
				}
				else {
					for time in &config.sleep_times {
						if time.hour() == now.hour() && time.minute() == now.minute() {
							tweaker.set_sleeping(&state.queue, true, false).await;
						}
					}
				}
			}
		});
	}

	pub(crate) async fn tweak(
		&self, length: usize, start: Option<String>, end: Option<String>
	) -> Result<String, tweakov::MarkovError> {
		let start = start.map(|text| text.into_bytes());
		let end = end.map(|text| text.into_bytes());
		let inner = self.inner.read().await;
		let text = inner.markov.generate_text(length, start, end)?;
		Ok(String::from_utf8_lossy(&text).to_string())
	}

	pub(crate) async fn apply_filters(&self, string: String, fd: Vec<FilterData>)
	-> crate::Result<String> {
		let inner = self.inner.read().await;
		inner.filters.apply(string, fd)
	}

	pub(crate) async fn tweak_filtered(
		&self, config: &TweakerConfig, length: usize,
		start: Option<String>, end: Option<String>
	) -> crate::Result<String> {
		let slen = start.as_ref().map(|s| s.len());

		let mut text = String::new();
		while text.is_empty() {
			text = self.tweak(length, start.clone(), end.clone()).await?;
			if config.remove_start_text && let Some(len) = slen {
				text = text[len..].to_string();
			}
			text = self.apply_filters(text, config.filters.clone()).await?;
		}

		Ok(text)
	}

	pub(crate) fn tweak_params(config: &TweakerConfig, body: &str, keyword: Option<Keyword>)
	-> (usize, Option<String>, Option<String>) {
		let mut rng = rng();
		let length = rng.random_range(config.length_min..=config.length_max);
		let start = keyword.filter(|k| k.start_on())
			.map(|k| k.keyword())
			.or_else(|| {
				body.split(' ')
					.map(|s| s.to_lowercase().to_string())
					.filter(|word| !config.ignore_words.contains(word))
					.choose(&mut rng)
			});
		let end = config.endings.choose(&mut rng).cloned();
		(length, start, end)
	}

	pub(crate) fn get_reaction(config: &TweakerConfig) -> String {
		let max = config.reactions.iter().fold(0.0, |a, r| a + r.1);
		let mut rand = rng().random_range(0.0f64..max);
		for (re, prob) in &config.reactions {
			if rand <= *prob {
				return re.clone();
			}
			rand -= *prob;
		}
		String::new()
	}

	pub(crate) fn choose_action(
		config: &TweakerConfig, props: &MessageProps, mode: TweakMode, td: TimeDelta
	) -> (TweakAction, bool) {
		if mode == TweakMode::Disabled {
			return (TweakAction::None, false);
		}

		let mut rng = rng();
		let mut action = TweakAction::None;

		if mode != TweakMode::Reactions {
			// always reply to keywords
			if props.keyword.is_some() {
				let rand = rng.random_range(0.0f64..1.0);
				if rand <= config.prob_tweak_keyword {
					action = TweakAction::Reply;
				}
			}
			else if mode != TweakMode::KeywordsOnly {
				// check cooldown
				if td < TimeDelta::seconds(config.cooldown) {
					return (TweakAction::None, true);
				}

				// should reply to replies most of the time
				if props.reply {
					let rand = rng.random_range(0.0f64..1.0);
					if rand <= config.prob_tweak_reply {
						action = TweakAction::Reply;
					}
				}
				// otherwise a normal message
				else if mode != TweakMode::KeywordsAndReplies {
					let rand = rng.random_range(0.0f64..1.0);
					if rand <= config.prob_tweak {
						let rand = rng.random_range(0.0f64..1.0);
						if rand <= config.prob_reply {
							action = TweakAction::Reply;
						}
						else {
							action = TweakAction::Message;
						}
					}
				}
			}
		}

		// if not doing anything, maybe still react
		if action == TweakAction::None {
			if td <  TimeDelta::seconds(config.cooldown_react) {
				return (TweakAction::None, true);
			}

			let rand = rng.random_range(0.0f64..1.0);
			if rand <= config.prob_react {
				action = TweakAction::React;
			}
		}

		(action, false)
	}

	pub(crate) async fn tweak_on_message(
		&self, state: State, room: Room,
		message: OriginalSyncRoomMessageEvent, props: MessageProps
	) -> crate::Result<Response> {
		let body = message.content.body();
		let config = state.config().await.tweaker;
		self.ingest(&config, body.bytes().collect::<Vec<u8>>()).await;

		if config.do_not_tweak.unwrap_or(false) {
			tracing::debug!("do_not_tweak == true");
			return Ok(Response::None);
		}

		let room_id = room.room_id().as_str();
		let mode = config.tweak_mode(room_id);

		if mode == TweakMode::Disabled {
			tracing::debug!("tweaking disabled in {room_id}");
			return Ok(Response::None);
		}

		let mut inner = self.inner.write().await;
		let last = inner.last_message.get(room_id).cloned()
				.unwrap_or(DateTime::UNIX_EPOCH);
		inner.last_message.insert(room_id.to_string(), props.timestamp);

		drop(inner);

		let (action, cooldown) = Self::choose_action(&config, &props, mode, props.now - last);
		if cooldown {
			tracing::debug!("cooldown in {room_id}");
			return Ok(Response::None);
		}

		match action {
			TweakAction::None => Ok(Response::None),
			TweakAction::Message | TweakAction::Reply => {
				tracing::info!("[{room_id}] i want to tweak! now! here!");
				let (length, start, end) = Self::tweak_params(&config, body, props.keyword);
				let text = self.tweak_filtered(&config, length, start, end).await?;
				tracing::info!("{text}");
				let reply = (action == TweakAction::Reply).then_some(&message);
				Ok(Response::message(&text, reply))
			},
			TweakAction::React => {
				let reaction = Self::get_reaction(&config);
				tracing::info!("[{room_id}] my reaction to that information: {reaction}");
				Ok(Response::reaction(message.event_id, reaction))
			}
		}
	}
}

impl Clone for Tweaker {
	fn clone(&self) -> Self {
		Self {
			inner: self.inner.clone()
		}
	}
}
