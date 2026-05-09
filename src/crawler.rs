use crate::{Config, State, bot::{http_client, join_or_knock}};
use std::{fs::File, io::Write, path::PathBuf};
use serde::Deserialize;
use chrono::{DateTime, Datelike, Local};
use matrix_sdk::{
	Room,
	ruma::{
		OwnedRoomOrAliasId, RoomId,
		events::room::{
			MediaSource, message::{MessageType, OriginalSyncRoomMessageEvent}
		}
	}
};
use regex::Regex;

const ROOM_ID_REGEX: &str = r"![a-zA-Z0-9\-_.]{8,}(:[0-9a-z\-.]+)?|#[a-zA-Z0-9\-_.]+:[0-9a-z\-.]+";
const MEDIA_DOWNLOAD_URI: &str = "/_matrix/media/r0/download";

#[derive(Deserialize, Clone)]
pub(crate) struct CrawlerConfig {
	log_directory: PathBuf,
	enable_media_downloads: Option<bool>,
	media_directory: PathBuf,
	media_download_server: String,
	media_download_uri: Option<String>,
	join_rooms: bool
}

fn dir_ts(mut path: PathBuf, ts: DateTime<Local>) -> Result<PathBuf, std::io::Error> {
	path.push(ts.year().to_string());
	path.push(ts.month().to_string());
	path.push(ts.day().to_string());
	std::fs::create_dir_all(&path)?;
	Ok(path)
}

fn message_filename(
	message: &OriginalSyncRoomMessageEvent, ts: DateTime<Local>, room_id: &RoomId
) -> String {
	format!("{}{}{room_id}{}", ts.time(), message.sender, message.event_id)
}

async fn download_media(
	config: &Config, message: &OriginalSyncRoomMessageEvent,
	ts: DateTime<Local>, room_id: &RoomId
) -> crate::Result<()> {
	let (source, name) = match message.content.msgtype.clone() {
		MessageType::Audio(audio) => (audio.source, audio.filename),
		MessageType::File(file) => (file.source, file.filename),
		MessageType::Image(image) => (image.source, image.filename),
		MessageType::Video(video) => (video.source, video.filename),
		_ => { return Ok(()); }
	};

	if let MediaSource::Plain(mxc) = source {
		let server_name = mxc.server_name()?;
		let media_id = mxc.media_id()?;
		let download_uri = config.crawler.media_download_uri.clone()
			.unwrap_or(MEDIA_DOWNLOAD_URI.to_string());
		let uri = format!(
			"{}{download_uri}/{server_name}/{media_id}",
			config.crawler.media_download_server
		);

		let client = http_client(config)?;
		let res = client.get(uri).send().await?;
		let bytes = res.bytes().await?;

		let name = name.unwrap_or(media_id.to_string());
		let mut filename = message_filename(message, ts, room_id);
		filename.push('_');
		filename.push_str(&name);

		let mut path = dir_ts(config.crawler.media_directory.clone(), ts)?;
		path.push(filename);
		let mut file = File::create(path)?;
		file.write_all(&bytes)?;
	}

	Ok(())
}

pub(crate) fn log_message(config: &Config, room: &Room, message: &OriginalSyncRoomMessageEvent)
-> crate::Result<()> {
	let ts = message.origin_server_ts.as_secs();
	let ts = DateTime::from_timestamp_secs(ts.into())
		.unwrap_or_default()
		.with_timezone(&Local);

	if config.crawler.enable_media_downloads.unwrap_or(true) {
		let dm_config = config.clone();
		let dm_message = message.clone();
		let room_id = room.room_id().to_owned();
		tokio::spawn(async move {
			if let Err(err) = download_media(&dm_config, &dm_message, ts, &room_id).await {
				tracing::error!("error downloading media for {}: {err}", dm_message.event_id);
			}
		});
	}

	let body = message.content.body();
	if body.is_empty() {
		return Ok(());
	}

	let mut path = dir_ts(config.crawler.log_directory.clone(), ts)?;
	let name = format!("{}{}{}{}", ts.time(), message.sender, room.room_id(), message.event_id);
	path.push(name);
	let mut file = File::create(path)?;
	file.write_all(body.as_bytes())?;
	Ok(())
}

pub(crate) async fn find_and_join_rooms(state: &State, body: &str)
-> crate::Result<()> {
	let config = state.config().await;
	if !config.crawler.join_rooms {
		return Ok(());
	}

	let regex = Regex::new(ROOM_ID_REGEX)?;
	for room_id in regex.find_iter(body) {
		let room_id = OwnedRoomOrAliasId::try_from(room_id.as_str())?;
		if let Err(err) = join_or_knock(state, room_id.clone()).await {
			tracing::error!("Error joining room {room_id}: {err}");
		}
	}
	Ok(())
}
