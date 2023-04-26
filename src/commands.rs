use backon::{
	ExponentialBuilder,
	Retryable,
};
use chrono::Datelike;
use miette::{
	Diagnostic,
	IntoDiagnostic,
	Result,
	WrapErr,
};
use serenity::{
	builder::CreateEmbed,
	model::{
		application::interaction::application_command::ApplicationCommandInteraction,
		prelude::{
			command::{
				Command,
				CommandOptionType,
				CommandType,
			},
			interaction::InteractionResponseType::ChannelMessageWithSource,
		},
	},
	prelude::Context,
};
use thiserror::Error;
use tokio::select;
use tokio_util::sync::CancellationToken;
use tracing::{
	debug,
	instrument,
	warn,
};

use crate::{
	mensa::api::{
		CanteenData,
		Classifier,
		Line,
	},
	MensaApiKey,
	TurnOverKey,
};

const CANTEEN_LIST: &[(&str, &str)] = &[
	("KIT Campus", "mensa_adenauerring"),
	("Gottesaue", "mensa_gottesaue"),
	("Moltke", "mensa_moltke"),
	("Moltke 30", "mensa_x1moltkestrasse"),
	("Erzberger", "mensa_erzberger"),
	("Tiefbronner", "mensa_tiefenbronner"),
	("Holzgarten", "mensa_holzgarten"),
];

pub async fn register(ctx: &Context, cancel: CancellationToken) -> Result<()> {
	// spawn background worker in tokio runtime for dealing with update
	let ctx = ctx.clone();
	tokio::spawn(async move {
		let f = || async { register_slash_commands(&ctx).await };

		select! {
			_ = cancel.cancelled() => {
				warn!("Slash command registration cancelled.");
			}
			_ = f.retry(&ExponentialBuilder::default()) => {
				debug!("Successfully registered slash commands.");
			}
		}
	});

	Ok(())
}

async fn register_slash_commands(ctx: &Context) -> Result<()> {
	Command::set_global_application_commands(&ctx.http, |cmds| {
		cmds.create_application_command(|c| {
			c.name("mensa")
				.description("Zeige den Speiseplan der Mensa an.")
				.dm_permission(true)
				.kind(CommandType::ChatInput)
				.create_option(|o| {
					o.name("tag")
						.description("Tag für den der Speiseplan angezeigt werden soll.")
						.kind(CommandOptionType::String)
						.required(false)
						.add_string_choice("Heute", "today")
						.add_string_choice("Morgen", "tomorrow")
						.add_string_choice("Übermorgen", "dayaftertomorrow")
						.add_string_choice("Montag", "monday")
						.add_string_choice("Dienstag", "tuesday")
						.add_string_choice("Mittwoch", "wednesday")
						.add_string_choice("Donnerstag", "thursday")
						.add_string_choice("Freitag", "friday")
				})
				.create_option(|o| {
					o.name("kantine")
						.description("Kantine für die der Speiseplan angezeigt werden soll.")
						.kind(CommandOptionType::String)
						.required(false);

					for (name, value) in CANTEEN_LIST {
						o.add_string_choice(name, value);
					}
					o
				})
		})
	})
	.await
	.into_diagnostic()
	.wrap_err("Failed to create application commands.")?;

	Ok(())
}

#[instrument(
	skip_all,
	fields(
		user = ?interaction.user,
		interaction.data = ?interaction.data,
	)
)]
pub async fn handle_application_command(ctx: Context, interaction: ApplicationCommandInteraction) -> Result<()> {
	match interaction.data.name.as_str() {
		"mensa" => {
			handle_mensa_command(ctx, interaction).await?;
			Ok(())
		},
		_ => Err(UnknownCommandError {
			name: interaction.data.name,
		})
		.into_diagnostic(),
	}
}

#[derive(PartialEq)]
enum DayCorrection {
	/// Request could be fully processed without any correction to the date.
	Same,

	/// Canteen is past roll-over time, skipped to next day.
	RollOver,

	/// Canteen is closed on the given day, skipped to next open day.
	DaysSkipped,
}

async fn handle_mensa_command(ctx: Context, interaction: ApplicationCommandInteraction) -> Result<()> {
	let data = ctx.data.read().await;
	let api = data.get::<MensaApiKey>().unwrap();
	let roll_over_time = data.get::<TurnOverKey>().unwrap().0;

	let mut day_correction = DayCorrection::Same;

	// if argument is given, parse it, otherwise use current date but add one day if it is past roll-over time
	let lookup_date = interaction
		.data
		.options
		.iter()
		.find(|option| option.name == "tag")
		.map(|option| {
			let value = option.value.as_ref().unwrap().as_str().unwrap();
			parse_day_argument(value)
		})
		.unwrap_or_else(|| {
			// check if current time is past roll-over time, if so, add one day to the date
			let mut lookup_date = chrono::Local::now().naive_local();
			if lookup_date.time() > roll_over_time {
				lookup_date += chrono::Duration::days(1);
				day_correction = DayCorrection::RollOver;
			}
			Ok(lookup_date.date())
		})?;

	// check if day matches available plans, if not, find following day
	let mut available_plans = api.get_available_plans().await?;
	available_plans.sort();
	let plan = available_plans.into_iter().find(|plan| plan >= &lookup_date);

	// if no plan remains, we inform user that no plan is available
	if plan.is_none() {
		interaction
			.create_interaction_response(&ctx.http, |r| {
				r.kind(ChannelMessageWithSource).interaction_response_data(|d| d.content("No menu available."))
			})
			.await
			.into_diagnostic()?;
		return Ok(());
	}
	let plans = plan.unwrap();

	// if selected date does not match lookup date, we inform user that we skipped to the next available date
	if plans != lookup_date {
		day_correction = DayCorrection::DaysSkipped;
	}

	let menu = api.get_canteen_data(&plans).await?;
	// take first menu, as we only have one canteen TODO fix
	let canteen = menu.get(0).unwrap();

	// print available menu
	interaction
		.create_interaction_response(&ctx.http, |r| {
			r.kind(ChannelMessageWithSource).interaction_response_data(|d| {
				match day_correction {
					DayCorrection::RollOver => {
						d.content("Die Mensa ist geschlossen. Ich habe dir den nächsten Tag ausgewählt.");
					},
					DayCorrection::DaysSkipped => {
						d.content("An dem ausgewählten Tag ist die Mensa geschlossen. Ich habe dir den nächsten Tag ausgewählt.");
					},
					_ => {},
				};
				d.embed(|e| build_embed(e, canteen))
			})
		})
		.await
		.into_diagnostic()?;

	Ok(())
}

fn build_embed<'a>(embed: &'a mut CreateEmbed, canteen: &CanteenData) -> &'a mut CreateEmbed {
	embed
		.title(format!("Mensaeinheitsbrei für {} am {}", canteen.canteen.name, weekday_to_string(canteen.date.weekday())))
		.color(0x6f00ff)
		.footer(|f| f.text("Klick auf mein Profilbild und lad mich zu deinem Server ein!"));

	for line in &canteen.lines {
		// skip empty lines
		if line.meals.is_empty() {
			continue;
		}

		embed.field(line.name.as_str(), format_line(line), true);
	}

	embed
}

fn format_line(line: &Line) -> String {
	line
		.meals
		.iter()
		.filter(|m| {
			// filter out meals with empty price
			!m.price.is_empty()
		})
		.map(|meal| format!("{}{} ({})", emojiy_classifier(&meal.classifiers), meal.name, meal.price))
		.collect::<Vec<String>>()
		.join("\n")
}

fn weekday_to_string(weekday: chrono::Weekday) -> &'static str {
	match weekday {
		chrono::Weekday::Mon => "Montag",
		chrono::Weekday::Tue => "Dienstag",
		chrono::Weekday::Wed => "Mittwoch",
		chrono::Weekday::Thu => "Donnerstag",
		chrono::Weekday::Fri => "Freitag",
		_ => "Unbekannt",
	}
}

fn parse_day_argument(arg: &str) -> Result<chrono::NaiveDate> {
	let now = chrono::Local::now().naive_local();

	// parse day argument
	let date = match arg {
		"today" => now.date(),
		"tomorrow" => now.date() + chrono::Duration::days(1),
		"dayaftertomorrow" => now.date() + chrono::Duration::days(2),
		"monday" => next_weekday(chrono::Weekday::Mon),
		"tuesday" => next_weekday(chrono::Weekday::Tue),
		"wednesday" => next_weekday(chrono::Weekday::Wed),
		"thursday" => next_weekday(chrono::Weekday::Thu),
		"friday" => next_weekday(chrono::Weekday::Fri),
		_ => {
			return Err(InvalidDayArgumentError {
				arg: arg.to_string(),
			})
			.into_diagnostic();
		},
	};

	Ok(date)
}

/// Returns the next `naive_date` that is the given `weekday`.
/// If today is the given `weekday`, the current date is returned.
fn next_weekday(weekday: chrono::Weekday) -> chrono::NaiveDate {
	let now = chrono::Local::now().naive_local();
	let today = now.date();
	let days_to_add = (weekday.number_from_monday() + 7 - today.weekday().number_from_monday()) % 7;
	today + chrono::Duration::days(days_to_add as i64)
}

fn emojiy_classifier(classifier: &[Classifier]) -> &'static str {
	// map each classifier to emoji
	// group classifiers by type (beef, pork, ...) to same emoji
	let mut classifier = Vec::from(classifier);
	classifier.sort();
	classifier
		.iter()
		.map(|c| match c {
			Classifier::Pork | Classifier::OrganicPork => "🐖",
			Classifier::Beef | Classifier::OrganicBeef => "🐄",
			Classifier::Gelatine => "🐈",
			Classifier::Fish => "🐟",
			Classifier::Vegetarian => "🥕",
			Classifier::MensaVital => "🥦",
			Classifier::Vegan => "🌱",
			_ => "",
		})
		.next()
		.unwrap_or("")
}

#[derive(Error, Diagnostic, Debug)]
#[error("UnkownCommand")]
pub struct UnknownCommandError {
	name: String,
}

#[derive(Error, Diagnostic, Debug)]
#[error("InvalidDayArgument")]
pub struct InvalidDayArgumentError {
	arg: String,
}
