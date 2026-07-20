//! Read-only Wikimedia APIs used during moderation.

use anyhow::{Context, Result, bail};
use reqwest::header::{ACCEPT, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use url::Url;

const CACHE_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_CACHE_ENTRIES: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coordinates {
    pub latitude: f64,
    pub longitude: f64,
}

impl Coordinates {
    pub fn new(latitude: f64, longitude: f64) -> Option<Self> {
        let valid_latitude = latitude.is_finite() && (-90.0..=90.0).contains(&latitude);
        let valid_longitude = longitude.is_finite() && (-180.0..=180.0).contains(&longitude);
        (valid_latitude && valid_longitude).then_some(Self {
            latitude,
            longitude,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NearbyCategory {
    pub category: String,
    pub label: Option<String>,
    pub distance_km: Option<f64>,
}

#[derive(Clone)]
pub struct CommonsClient {
    client: reqwest::Client,
    user_agent: String,
    api_url: String,
    sparql_url: String,
    radius_meters: u32,
    nearby_limit: u32,
    category_cache: Arc<Mutex<HashMap<String, CacheEntry<Vec<String>>>>>,
    nearby_cache: Arc<Mutex<HashMap<String, CacheEntry<Vec<NearbyCategory>>>>>,
}

#[derive(Clone)]
struct CacheEntry<T> {
    inserted_at: Instant,
    value: T,
}

impl CommonsClient {
    pub fn new(
        user_agent: impl Into<String>,
        api_url: impl Into<String>,
        sparql_url: impl Into<String>,
        radius_meters: u32,
        nearby_limit: u32,
    ) -> Result<Self> {
        let user_agent = user_agent.into();
        let client = reqwest::Client::builder()
            .user_agent(&user_agent)
            .timeout(Duration::from_secs(20))
            .build()?;
        Ok(Self {
            client,
            user_agent,
            api_url: api_url.into(),
            sparql_url: sparql_url.into(),
            radius_meters,
            nearby_limit: nearby_limit.clamp(1, 20),
            category_cache: Arc::new(Mutex::new(HashMap::new())),
            nearby_cache: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub async fn search_categories(&self, raw_query: &str) -> Result<Vec<String>> {
        let query = normalize_category(raw_query);
        if query.chars().count() < 2 {
            return Ok(Vec::new());
        }
        let cache_key = query.to_lowercase();
        if let Some(cached) = cache_get(&self.category_cache, &cache_key).await {
            return Ok(cached);
        }

        let mut prefix_url = Url::parse(&self.api_url).context("invalid Commons API URL")?;
        prefix_url
            .query_pairs_mut()
            .append_pair("action", "query")
            .append_pair("format", "json")
            .append_pair("formatversion", "2")
            .append_pair("list", "prefixsearch")
            .append_pair("pssearch", &format!("Category:{query}"))
            .append_pair("psnamespace", "14")
            .append_pair("pslimit", "10");
        let prefix: PrefixResponse = self.get_json(prefix_url).await?;
        let mut candidates = prefix
            .query
            .prefixsearch
            .into_iter()
            .map(|hit| strip_category_prefix(&hit.title))
            .collect::<Vec<_>>();

        if candidates.len() < 5 {
            let mut search_url = Url::parse(&self.api_url).context("invalid Commons API URL")?;
            search_url
                .query_pairs_mut()
                .append_pair("action", "query")
                .append_pair("format", "json")
                .append_pair("formatversion", "2")
                .append_pair("list", "search")
                .append_pair("srsearch", &query)
                .append_pair("srnamespace", "14")
                .append_pair("srlimit", "10")
                .append_pair("srprop", "");
            let search: SearchResponse = self.get_json(search_url).await?;
            candidates.extend(
                search
                    .query
                    .search
                    .into_iter()
                    .map(|hit| strip_category_prefix(&hit.title)),
            );
        }

        let mut seen = HashSet::new();
        candidates.retain(|category| !category.is_empty() && seen.insert(category.to_lowercase()));
        candidates.truncate(10);
        cache_put(&self.category_cache, cache_key, candidates.clone()).await;
        Ok(candidates)
    }

    pub async fn nearby_categories(&self, coordinates: Coordinates) -> Result<Vec<NearbyCategory>> {
        let cache_key = format!("{:.4}:{:.4}", coordinates.latitude, coordinates.longitude);
        if let Some(cached) = cache_get(&self.nearby_cache, &cache_key).await {
            return Ok(cached);
        }
        let query = self.wikidata_query(coordinates);
        let response = self
            .client
            .get(&self.sparql_url)
            .header(USER_AGENT, &self.user_agent)
            .header("Api-User-Agent", &self.user_agent)
            .header(ACCEPT, "application/sparql-results+json")
            .query(&[("query", query.as_str()), ("format", "json")])
            .send()
            .await
            .context("Wikidata nearby-category request failed")?
            .error_for_status()
            .context("Wikidata nearby-category request returned an error")?
            .json::<SparqlResponse>()
            .await
            .context("Wikidata nearby-category response was invalid")?;
        let categories = response.into_categories(self.nearby_limit);
        cache_put(&self.nearby_cache, cache_key, categories.clone()).await;
        Ok(categories)
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: Url) -> Result<T> {
        self.client
            .get(url)
            .send()
            .await
            .context("Commons API request failed")?
            .error_for_status()
            .context("Commons API returned an error")?
            .json()
            .await
            .context("Commons API response was invalid")
    }

    fn wikidata_query(&self, coordinates: Coordinates) -> String {
        let longitude = decimal(coordinates.longitude);
        let latitude = decimal(coordinates.latitude);
        let radius_km = decimal(self.radius_meters as f64 / 1000.0);
        let item_limit = self.nearby_limit.saturating_mul(4).max(self.nearby_limit);
        format!(
            r#"SELECT ?item ?itemLabel ?commonsCategory ?distance WHERE {{
	SERVICE wikibase:around {{
		?item wdt:P625 ?location .
		bd:serviceParam wikibase:center "Point({longitude} {latitude})"^^geo:wktLiteral .
		bd:serviceParam wikibase:radius "{radius_km}" .
		bd:serviceParam wikibase:distance ?distance .
	}}
	?item wdt:P373 ?commonsCategory .
	SERVICE wikibase:label {{ bd:serviceParam wikibase:language "ru,be,en,uk,pl,de,fr,mul" . }}
}}
ORDER BY ASC(?distance)
LIMIT {item_limit}"#
        )
    }
}

pub fn normalize_category(raw: &str) -> String {
    let raw = raw.trim().replace('_', " ");
    let without_namespace = raw
        .strip_prefix("Category:")
        .or_else(|| raw.strip_prefix("category:"))
        .unwrap_or(&raw);
    without_namespace
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn validate_category(raw: &str) -> Result<String> {
    let category = normalize_category(raw);
    let forbidden = |character: char| {
        character.is_control()
            || character == '\u{fffd}'
            || matches!(character, '#' | '<' | '>' | '[' | ']' | '|' | '{' | '}')
    };
    if category.is_empty()
        || category.len() > 255
        || category.starts_with(':')
        || matches!(category.as_str(), "." | "..")
        || category.contains("~~~")
        || category.chars().any(forbidden)
    {
        bail!("a valid Wikimedia Commons category is required");
    }
    Ok(category)
}

async fn cache_get<T: Clone>(
    cache: &Mutex<HashMap<String, CacheEntry<T>>>,
    key: &str,
) -> Option<T> {
    let mut cache = cache.lock().await;
    cache.retain(|_, entry| entry.inserted_at.elapsed() < CACHE_TTL);
    cache.get(key).map(|entry| entry.value.clone())
}

async fn cache_put<T>(cache: &Mutex<HashMap<String, CacheEntry<T>>>, key: String, value: T) {
    let mut cache = cache.lock().await;
    if cache.len() >= MAX_CACHE_ENTRIES
        && let Some(oldest) = cache
            .iter()
            .min_by_key(|(_, entry)| entry.inserted_at)
            .map(|(key, _)| key.clone())
    {
        cache.remove(&oldest);
    }
    cache.insert(
        key,
        CacheEntry {
            inserted_at: Instant::now(),
            value,
        },
    );
}

fn strip_category_prefix(title: &str) -> String {
    title
        .strip_prefix("Category:")
        .unwrap_or(title)
        .trim()
        .to_string()
}

#[derive(Debug, Deserialize)]
struct PrefixResponse {
    query: PrefixQuery,
}

#[derive(Debug, Deserialize)]
struct PrefixQuery {
    #[serde(default)]
    prefixsearch: Vec<TitleHit>,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    query: SearchQuery,
}

#[derive(Debug, Deserialize)]
struct SearchQuery {
    #[serde(default)]
    search: Vec<TitleHit>,
}

#[derive(Debug, Deserialize)]
struct TitleHit {
    title: String,
}

#[derive(Debug, Deserialize)]
struct SparqlResponse {
    results: SparqlResults,
}

impl SparqlResponse {
    fn into_categories(self, limit: u32) -> Vec<NearbyCategory> {
        let mut seen = HashSet::new();
        let mut categories = self
            .results
            .bindings
            .into_iter()
            .filter_map(SparqlBinding::into_category)
            .filter(|candidate| seen.insert(candidate.category.to_lowercase()))
            .collect::<Vec<_>>();
        categories
            .sort_by(|left, right| optional_distance_order(left.distance_km, right.distance_km));
        categories.truncate(limit as usize);
        categories
    }
}

#[derive(Debug, Deserialize)]
struct SparqlResults {
    #[serde(default)]
    bindings: Vec<SparqlBinding>,
}

#[derive(Debug, Deserialize)]
struct SparqlBinding {
    #[serde(rename = "itemLabel")]
    item_label: Option<SparqlValue>,
    #[serde(rename = "commonsCategory")]
    commons_category: SparqlValue,
    distance: Option<SparqlValue>,
}

impl SparqlBinding {
    fn into_category(self) -> Option<NearbyCategory> {
        let category = normalize_category(&self.commons_category.value);
        if category.is_empty() {
            return None;
        }
        Some(NearbyCategory {
            label: self
                .item_label
                .map(|value| value.value.trim().to_string())
                .filter(|value| !value.is_empty() && value != &category),
            category,
            distance_km: self
                .distance
                .and_then(|value| value.value.parse::<f64>().ok()),
        })
    }
}

#[derive(Debug, Deserialize)]
struct SparqlValue {
    value: String,
}

fn decimal(value: f64) -> String {
    let mut text = format!("{value:.6}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    text
}

fn optional_distance_order(left: Option<f64>, right: Option<f64>) -> std::cmp::Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left
            .partial_cmp(&right)
            .unwrap_or(std::cmp::Ordering::Equal),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CommonsClient, Coordinates, SparqlResponse, normalize_category, validate_category,
    };

    #[test]
    fn validates_coordinates() {
        assert!(Coordinates::new(53.9, 27.5667).is_some());
        assert!(Coordinates::new(91.0, 27.5667).is_none());
        assert!(Coordinates::new(53.9, 181.0).is_none());
    }

    #[test]
    fn normalizes_category_names() {
        assert_eq!(
            normalize_category(" Category:Buildings_in_Minsk "),
            "Buildings in Minsk"
        );
        assert_eq!(normalize_category("Belarus   churches"), "Belarus churches");
    }

    #[test]
    fn rejects_invalid_mediawiki_category_titles() {
        assert_eq!(
            validate_category("Category:Castles_in_Belarus").unwrap(),
            "Castles in Belarus"
        );
        for invalid in ["", "Foo#bar", "Bad[title]", "~~~", ":Hidden"] {
            assert!(validate_category(invalid).is_err(), "accepted {invalid:?}");
        }
        assert!(validate_category(&"Ж".repeat(128)).is_err());
    }

    #[test]
    fn query_uses_longitude_then_latitude_and_p373() {
        let client = CommonsClient::new(
            "test",
            "https://commons.wikimedia.org/w/api.php",
            "https://query.wikidata.org/sparql",
            10_000,
            20,
        )
        .unwrap();
        let query = client.wikidata_query(Coordinates::new(53.9, 27.5667).unwrap());
        assert!(query.contains("Point(27.5667 53.9)"));
        assert!(query.contains("?item wdt:P373 ?commonsCategory"));
        assert!(query.contains("wikibase:radius \"10\""));
        assert!(query.contains("LIMIT 80"));
    }

    #[test]
    fn parses_deduplicates_and_sorts_nearby_categories() {
        let response: SparqlResponse = serde_json::from_str(
            r#"{
                "results": {"bindings": [
                    {"itemLabel":{"value":"Minsk"},"commonsCategory":{"value":"Minsk"},"distance":{"value":"0.4"}},
                    {"itemLabel":{"value":"Again"},"commonsCategory":{"value":"Minsk"},"distance":{"value":"0.5"}},
                    {"itemLabel":{"value":"Church"},"commonsCategory":{"value":"Churches in Minsk"},"distance":{"value":"0.2"}}
                ]}
            }"#,
        )
        .unwrap();
        let categories = response.into_categories(20);
        assert_eq!(categories.len(), 2);
        assert_eq!(categories[0].category, "Churches in Minsk");
        assert_eq!(categories[1].category, "Minsk");
    }
}
