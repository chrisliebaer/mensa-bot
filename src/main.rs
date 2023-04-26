mod commands;
mod mensa;

use std::{
	str::FromStr,
	sync::atomic::{
		AtomicBool,
		Ordering::SeqCst,
	},
};

use async_trait::async_trait;
use chrono::NaiveTime;
use cron::Schedule;
use envconfig::Envconfig;
use miette::{
	IntoDiagnostic,
	Result,
	WrapErr,
};
use serenity::{
	client::{
		Context,
		EventHandler,
	},
	model::{
		application::interaction::Interaction,
		gateway::Ready,
	},
	prelude::{
		GatewayIntents,
		TypeMapKey,
	},
};
use tokio::signal;
use tokio_util::sync::CancellationToken;
use tracing::{
	error,
	info,
	warn,
};

use crate::mensa::api::MensaApi;

#[derive(Envconfig)]
struct Config {
	/// The bot token to use.
	#[envconfig(from = "BOT_TOKEN")]
	pub bot_token: String,

	/// The endpoint to query for mensa menu.
	#[envconfig(from = "API_URL")]
	pub api_url: String,

	/// Optional channel to announce the menu in and keep updated.
	#[envconfig(from = "ANNOUNCE_CHANNEL")]
	pub announce_channel: Option<String>,

	/// Cron expression for when when to run update check.
	#[envconfig(from = "ANNOUNCE_CRON")]
	pub announce_cron: Option<Schedule>,

	/// Time of day when we consider mensa closed and will reply with menu for
	/// next day.
	#[envconfig(from = "NEXT_DAY")]
	pub next_day: TimeWrapper,
}

struct TimeWrapper(NaiveTime);

impl FromStr for TimeWrapper {
	type Err = chrono::ParseError;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		Ok(Self(NaiveTime::parse_from_str(s, "%H:%M")?))
	}
}

impl From<TimeWrapper> for NaiveTime {
	fn from(t: TimeWrapper) -> Self {
		t.0
	}
}

struct ApplicationCancelTokenKey;
impl TypeMapKey for ApplicationCancelTokenKey {
	type Value = CancellationToken;
}

struct MensaApiKey;
impl TypeMapKey for MensaApiKey {
	type Value = MensaApi;
}

struct TurnOverKey;
impl TypeMapKey for TurnOverKey {
	type Value = TimeWrapper;
}

#[tokio::main]
async fn main() -> Result<()> {
	tracing_subscriber::fmt::init();
	let config = Config::init_from_env().into_diagnostic().wrap_err("Failed to load config from environment variables.")?;

	let intents = GatewayIntents::empty();
	let mut client = serenity::Client::builder(&config.bot_token, intents)
		.event_handler(Handler::default())
		.await
		.into_diagnostic()
		.wrap_err("Failed to create Discord client.")?;

	// instance application resources
	let mut data = client.data.write().await;
	let cancel_token = CancellationToken::new();
	data.insert::<ApplicationCancelTokenKey>(cancel_token.clone());
	let mensa_api_url = config.api_url.parse().into_diagnostic().wrap_err("Failed to parse API URL.")?;
	let api = MensaApi::new(mensa_api_url);
	data.insert::<MensaApiKey>(api);
	data.insert::<TurnOverKey>(config.next_day);
	drop(data);

	{
		let cancel_token = cancel_token.clone();
		tokio::spawn(async move {
			tokio::select! {
				_ = cancel_token.cancelled() => {
					info!("Application cancelled.");
				},
				result = client.start() => {
					match result {
						Ok(_) => info!("Discord client stopped."),
						Err(e) => warn!("Discord client stopped with error: {}", e),
					}
					info!("Sending cancellation signal.");
				},
			}
		});
	}

	match signal::ctrl_c().await {
		Ok(()) => {},
		Err(err) => {
			error!("Failed to listen for ctrl-c: {}", err);
		},
	}

	cancel_token.cancel();

	Ok(())
}

#[derive(Debug)]
struct Handler {
	first_ready: AtomicBool,
}

impl Handler {
	pub fn default() -> Self {
		Self {
			first_ready: AtomicBool::new(true),
		}
	}
}

#[async_trait]
impl EventHandler for Handler {
	async fn ready(&self, ctx: Context, ready: Ready) {
		info!("Connected as '{}' serving {} guilds.", ready.user.name, ready.guilds.len().to_string());

		if self.first_ready.compare_exchange(true, false, SeqCst, SeqCst).is_ok() {
			let data = ctx.data.read().await;
			let cancel_token = data.get::<ApplicationCancelTokenKey>().unwrap();
			if let Err(e) = commands::register(&ctx, cancel_token.clone()).await {
				warn!("Failed to register command logic: {}", e);
			}
		}
	}

	async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
		// unpack application command
		let command = match interaction.application_command() {
			Some(command) => command,
			None => {
				warn!("Received interaction that is not an application command.");
				return;
			},
		};

		if let Err(e) = commands::handle_application_command(ctx, command).await {
			// print error using miette to get nice error messages
			warn!("Failed to handle application command: {:?}", e);
		}
	}
}
