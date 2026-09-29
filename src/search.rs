use std::sync::OnceLock;

use regex::Regex;

use crate::models::{Beatmap, BeatmapSet};

#[derive(Debug, Clone, PartialEq)]
pub struct SearchFilter {
    pub field: String,
    pub operator: String,
    pub value: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchQuery {
    pub text: String,
    pub filters: Vec<SearchFilter>,
}

impl SearchQuery {
    pub fn parse(input: &str) -> Self {
        if input.trim().is_empty() {
            return Self::default();
        }

        let regex = filter_regex();
        let filters = regex
            .captures_iter(input)
            .filter_map(|captures| {
                Some(SearchFilter {
                    field: captures.get(1)?.as_str().to_ascii_lowercase(),
                    operator: captures.get(2)?.as_str().to_owned(),
                    value: captures.get(3)?.as_str().parse().ok()?,
                })
            })
            .collect();
        let without_filters = regex.replace_all(input, "").replace('&', " ");
        let text = without_filters
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");

        Self { text, filters }
    }

    /// `mode` is an osu! API mode name; when set, only difficulties of that mode are considered.
    pub fn matches(&self, beatmap_set: &BeatmapSet, mode: Option<&str>) -> bool {
        self.filters.is_empty()
            || beatmap_set
                .beatmaps
                .iter()
                .filter(|beatmap| mode.is_none_or(|mode| beatmap.mode == mode))
                .any(|beatmap| self.filters.iter().all(|filter| filter.matches(beatmap)))
    }
}

impl SearchFilter {
    fn matches(&self, beatmap: &Beatmap) -> bool {
        let actual = match self.field.as_str() {
            "star" => beatmap.difficulty_rating,
            "bpm" => beatmap.bpm,
            "length" => f64::from(beatmap.total_length),
            "ar" => beatmap.ar,
            "cs" => beatmap.cs,
            "od" => beatmap.od,
            "hp" => beatmap.hp,
            _ => return true,
        };

        match self.operator.as_str() {
            ">=" => actual >= self.value,
            "<=" => actual <= self.value,
            ">" => actual > self.value,
            "<" => actual < self.value,
            "=" => (actual - self.value).abs() < 0.01,
            _ => true,
        }
    }
}

fn filter_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?i)\b(star|bpm|length|ar|cs|od|hp)\s*(>=|<=|>|<|=)\s*([0-9]+(?:\.[0-9]+)?)")
            .expect("search filter regex is valid")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_filters_and_normalizes_text() {
        let query = SearchQuery::parse("shuniki star>=5 & STAR <= 10");
        assert_eq!(query.text, "shuniki");
        assert_eq!(query.filters.len(), 2);
        assert_eq!(query.filters[0].value, 5.0);
        assert_eq!(query.filters[1].operator, "<=");
    }

    #[test]
    fn requires_one_difficulty_to_match_every_filter() {
        let query = SearchQuery::parse("star>=5 ar>=9");
        let mut set = BeatmapSet {
            beatmaps: vec![
                Beatmap {
                    difficulty_rating: 5.5,
                    ar: 8.0,
                    ..Default::default()
                },
                Beatmap {
                    difficulty_rating: 4.0,
                    ar: 9.5,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert!(!query.matches(&set, None));

        set.beatmaps.push(Beatmap {
            difficulty_rating: 6.0,
            ar: 9.5,
            ..Default::default()
        });
        assert!(query.matches(&set, None));
    }

    #[test]
    fn equality_uses_legacy_tolerance() {
        let query = SearchQuery::parse("bpm=180");
        let set = BeatmapSet {
            beatmaps: vec![Beatmap {
                bpm: 180.009,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(query.matches(&set, None));
    }

    #[test]
    fn filters_need_word_boundary_and_respect_mode() {
        let query = SearchQuery::parse("Teddy Bear <3");
        assert!(query.filters.is_empty());
        assert_eq!(query.text, "Teddy Bear <3");

        let query = SearchQuery::parse("star>=6");
        let set = BeatmapSet {
            beatmaps: vec![
                Beatmap {
                    mode: "osu".to_owned(),
                    difficulty_rating: 6.0,
                    ..Default::default()
                },
                Beatmap {
                    mode: "taiko".to_owned(),
                    difficulty_rating: 3.0,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert!(query.matches(&set, None));
        assert!(query.matches(&set, Some("osu")));
        assert!(!query.matches(&set, Some("taiko")));
    }
}
