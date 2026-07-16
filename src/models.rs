use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct AppSettings {
    pub client_id: String,
    pub client_secret: String,
    pub osu_path: String,
    pub prefer_no_video: bool,
    pub username: String,
    pub is_supporter: bool,
    pub support_level: i32,
    pub is_logged_in: bool,
    pub user_access_token: Option<String>,
    #[serde(with = "settings_expiry")]
    pub user_token_expiry: Option<DateTime<Utc>>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            client_secret: String::new(),
            osu_path: String::new(),
            prefer_no_video: true,
            username: String::new(),
            is_supporter: false,
            support_level: 0,
            is_logged_in: false,
            user_access_token: None,
            user_token_expiry: None,
        }
    }
}

impl AppSettings {
    pub fn is_configured(&self) -> bool {
        !self.client_id.trim().is_empty() && !self.client_secret.trim().is_empty()
    }

    pub fn osu_songs_path(&self) -> Option<std::path::PathBuf> {
        let path = self.osu_path.trim();
        if path.is_empty() {
            return None;
        }

        let root = std::path::PathBuf::from(path);
        if root
            .file_name()
            .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("Songs"))
        {
            Some(root)
        } else {
            Some(root.join("Songs"))
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BeatmapSet {
    pub id: i32,
    pub title: String,
    pub artist: String,
    pub creator: String,
    pub status: String,
    #[serde(with = "optional_datetime")]
    pub ranked_date: Option<DateTime<Utc>>,
    #[serde(with = "optional_datetime")]
    pub submitted_date: Option<DateTime<Utc>>,
    pub covers: BeatmapCovers,
    pub beatmaps: Vec<Beatmap>,
    #[serde(skip)]
    pub is_queued: bool,
    #[serde(skip)]
    pub is_downloaded: bool,
}

impl BeatmapSet {
    pub fn date_text(&self) -> String {
        self.ranked_date
            .or(self.submitted_date)
            .map(|date| date.format("%Y-%m-%d").to_string())
            .unwrap_or_default()
    }

    pub fn star_range_text(&self) -> String {
        if self.beatmaps.is_empty() {
            return "\u{2605} ?".to_owned();
        }

        let min = self
            .beatmaps
            .iter()
            .map(|beatmap| beatmap.difficulty_rating)
            .fold(f64::INFINITY, f64::min);
        let max = self
            .beatmaps
            .iter()
            .map(|beatmap| beatmap.difficulty_rating)
            .fold(f64::NEG_INFINITY, f64::max);
        format!("\u{2605} {min:.1} - {max:.1}")
    }

    pub fn cover_url(&self) -> Option<&str> {
        self.covers
            .list_2x
            .as_deref()
            .or(self.covers.list.as_deref())
            .or(self.covers.cover.as_deref())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BeatmapCovers {
    pub cover: Option<String>,
    #[serde(rename = "cover@2x")]
    pub cover_2x: Option<String>,
    pub list: Option<String>,
    #[serde(rename = "list@2x")]
    pub list_2x: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Beatmap {
    pub id: i32,
    pub difficulty_rating: f64,
    pub mode: String,
    pub bpm: f64,
    pub total_length: i32,
    pub ar: f64,
    pub cs: f64,
    #[serde(rename = "accuracy")]
    pub od: f64,
    #[serde(rename = "drain")]
    pub hp: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BeatmapSearchResponse {
    pub beatmapsets: Vec<BeatmapSet>,
    pub cursor_string: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OAuthTokenResponse {
    pub access_token: String,
    pub expires_in: i64,
    pub token_type: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OsuUser {
    pub id: i32,
    pub username: String,
    pub avatar_url: String,
    pub is_supporter: bool,
    pub support_level: i32,
}

fn parse_datetime(value: &str) -> Option<DateTime<Utc>> {
    if value.starts_with("0001-01-01") {
        return None;
    }
    if let Ok(value) = DateTime::parse_from_rfc3339(value) {
        return Some(value.with_timezone(&Utc));
    }

    ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"]
        .iter()
        .find_map(|format| NaiveDateTime::parse_from_str(value, format).ok())
        .map(|value| value.and_utc())
}

mod optional_datetime {
    use super::*;

    pub fn serialize<S>(value: &Option<DateTime<Utc>>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(value) => serializer.serialize_some(&value.to_rfc3339()),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<DateTime<Utc>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Option::<String>::deserialize(deserializer)?;
        Ok(value.as_deref().and_then(parse_datetime))
    }
}

mod settings_expiry {
    use super::*;

    pub fn serialize<S>(value: &Option<DateTime<Utc>>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(
            &value
                .map(|value| value.to_rfc3339())
                .unwrap_or_else(|| "0001-01-01T00:00:00".to_owned()),
        )
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<DateTime<Utc>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Option::<String>::deserialize(deserializer)?;
        Ok(value.as_deref().and_then(parse_datetime))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_legacy_settings_and_min_value_expiry() {
        let json = r#"{
            "ClientId":"123",
            "ClientSecret":"secret",
            "OsuPath":"C:\\\\osu!",
            "PreferNoVideo":true,
            "UserTokenExpiry":"0001-01-01T00:00:00",
            "IsConfigured":true
        }"#;

        let settings: AppSettings = serde_json::from_str(json).unwrap();
        assert!(settings.is_configured());
        assert_eq!(settings.user_token_expiry, None);
        assert!(settings.osu_songs_path().unwrap().ends_with("Songs"));
    }

    #[test]
    fn accepts_newtonsoft_computed_cache_fields() {
        let json = r#"{
            "id": 42,
            "title": "Title",
            "artist": "Artist",
            "DateText": "2026-01-01",
            "MinStarRating": 5.0,
            "covers": {"list@2x": "https://example.test/cover.jpg"},
            "beatmaps": [{"difficulty_rating": 5.25}]
        }"#;

        let set: BeatmapSet = serde_json::from_str(json).unwrap();
        assert_eq!(set.id, 42);
        assert_eq!(set.star_range_text(), "\u{2605} 5.2 - 5.2");
        assert_eq!(set.cover_url(), Some("https://example.test/cover.jpg"));
    }
}
