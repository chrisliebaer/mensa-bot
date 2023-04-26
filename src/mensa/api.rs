use chrono::NaiveDate;
use miette::{
	IntoDiagnostic,
	Result,
	WrapErr,
};
use reqwest::Url;
use serde::{
	Deserialize,
	Deserializer,
};

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiResult<T> {
	pub success: bool,
	pub data: T,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanteenData {
	#[serde(deserialize_with = "deserialize_date")]
	pub date: NaiveDate,
	pub canteen: Canteen,
	pub lines: Vec<Line>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Canteen {
	pub id: String,
	pub name: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Line {
	pub id: Option<String>,
	pub name: String,
	pub meals: Vec<Meal>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Meal {
	pub name: String,
	pub price: String,
	pub classifiers: Vec<Classifier>,
	pub additives: Vec<String>,
}

/// A classifier for a meal. The website lists the following classifiers:
/// [R] contains beef
/// [RAT] contains organically grown beef
/// [S] contains pork
/// [SAT] contains organically grown pork
/// [VEG] vegetarian dish
/// [VG] vegan dish
/// [MSC] MSC certified fish
/// [MV] MensaVital
/// [LAB] with animal rennet
/// [GEL] with gelatine
#[derive(Debug, Clone, PartialEq, PartialOrd, Ord, Eq, Deserialize)]
pub enum Classifier {
	#[serde(rename = "S")]
	Pork,

	#[serde(rename = "SAT")]
	OrganicPork,

	#[serde(rename = "R")]
	Beef,

	#[serde(rename = "RAT")]
	OrganicBeef,

	#[serde(rename = "GEL")]
	Gelatine,

	#[serde(rename = "MSC")]
	Fish,

	#[serde(rename = "LAB")]
	AnimalRennet,

	#[serde(rename = "VEG")]
	Vegetarian,

	#[serde(rename = "VG")]
	Vegan,

	#[serde(rename = "MV")]
	MensaVital,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NaiveDateWrapper {
	#[serde(deserialize_with = "deserialize_date")]
	pub date: NaiveDate,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Date {
	pub day: i64,
	pub month: i64,
	pub year: i64,
}

pub struct MensaApi {
	base_url: Url,
}

impl MensaApi {
	pub fn new(base_url: Url) -> Self {
		Self {
			base_url,
		}
	}
}

// implement deserialize for NaiveDate via deserialize_with using Date struct as a single function
fn deserialize_date<'de, D>(deserializer: D) -> Result<NaiveDate, D::Error>
where D: Deserializer<'de> {
	let date = Date::deserialize(deserializer)?;
	// month is indexed from 0, so we need to add 1
	NaiveDate::from_ymd_opt(date.year as i32, (date.month + 1) as u32, date.day as u32)
		.ok_or(serde::de::Error::custom("Invalid date."))
}

impl MensaApi {
	pub async fn get_available_plans(&self) -> Result<Vec<NaiveDate>> {
		let url = self.base_url.join("plans").into_diagnostic().wrap_err("Failed to construct url for available plans.")?;
		let response = reqwest::get(url).await.into_diagnostic().wrap_err("Failed to fetch available plans.")?;

		let data =
			response.json::<ApiResult<Vec<NaiveDateWrapper>>>().await.into_diagnostic().wrap_err("Failed to parse available plans.")?;

		let plans = data.data.into_iter().map(|plan| plan.date).collect();
		Ok(plans)
	}

	pub async fn get_canteen_data(&self, day: &NaiveDate) -> Result<Vec<CanteenData>> {
		// date needs to be in format YYYY-MM-DD
		let day = day.format("%Y-%m-%d").to_string();
		let url =
			self.base_url.join(&format!("plans/{}", day)).into_diagnostic().wrap_err("Failed to construct url for canteen data.")?;
		let response = reqwest::get(url).await.into_diagnostic().wrap_err("Failed to fetch canteen data.")?;

		let data =
			response.json::<ApiResult<Vec<CanteenData>>>().await.into_diagnostic().wrap_err("Failed to parse canteen data.")?;

		Ok(data.data)
	}
}
