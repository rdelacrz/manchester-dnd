use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use manchester_dnd_core::is_valid_opaque_id;
use serde::{Deserialize, Serialize};

use crate::error::EventPromptError;

const MAX_EVENT_PROMPT_BYTES: u64 = 64 * 1024;
const MAX_EVENT_PROMPTS: usize = 10_000;
const MAX_EVENT_WEIGHT: f64 = 1_000_000.0;
const MAX_EVENT_LABELS: usize = 64;
const MAX_EVENT_TITLE_CHARS: usize = 200;
const MAX_EVENT_COOLDOWN_TURNS: u64 = 1_000_000;

fn default_schema_version() -> u16 {
    1
}

fn default_weight() -> f64 {
    1.0
}

fn default_minimum_level() -> u8 {
    1
}

fn default_enabled() -> bool {
    false
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EventPromptMetadata {
    #[serde(default = "default_schema_version")]
    pub schema_version: u16,
    pub id: String,
    pub title: String,
    #[serde(default = "default_weight")]
    pub weight: f64,
    #[serde(default = "default_minimum_level")]
    pub minimum_level: u8,
    #[serde(default)]
    pub maximum_level: Option<u8>,
    #[serde(default)]
    pub cooldown_turns: u64,
    #[serde(default)]
    pub sensitivity_tags: Vec<String>,
    #[serde(default)]
    pub participant_aliases: Vec<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventPrompt {
    pub metadata: EventPromptMetadata,
    pub prompt: String,
    pub source_path: PathBuf,
}

impl EventPrompt {
    pub fn is_eligible(&self, context: &EventEligibility<'_>) -> bool {
        let metadata = &self.metadata;
        context.inspiration_enabled
            && metadata.enabled
            && context.party_level >= metadata.minimum_level
            && metadata
                .maximum_level
                .is_none_or(|maximum| context.party_level <= maximum)
            && metadata.sensitivity_tags.iter().all(|tag| {
                context
                    .allowed_sensitivity_tags
                    .contains(&normalize_label(tag))
            })
            && metadata.participant_aliases.iter().all(|alias| {
                context
                    .consenting_participant_aliases
                    .contains(&normalize_label(alias))
            })
            && context
                .last_triggered_turn
                .get(&metadata.id)
                .is_none_or(|last_turn| {
                    context.current_turn >= *last_turn
                        && context.current_turn - *last_turn >= metadata.cooldown_turns
                })
    }
}

#[derive(Debug)]
pub struct EventEligibility<'a> {
    /// Campaign-level opt-in. The selector returns no private inspiration when
    /// this is false, even if individual files are enabled.
    pub inspiration_enabled: bool,
    pub party_level: u8,
    pub current_turn: u64,
    /// Explicit allowlist. An event with any tag not in this set is ineligible.
    pub allowed_sensitivity_tags: &'a BTreeSet<String>,
    /// Explicit aliases of every represented participant who currently opts in.
    pub consenting_participant_aliases: &'a BTreeSet<String>,
    pub last_triggered_turn: &'a HashMap<String, u64>,
}

pub trait RandomSource {
    /// Returns a sample in the half-open interval `[0, 1)`.
    fn sample_unit(&mut self) -> f64;
}

#[derive(Debug, Default)]
pub struct ThreadRandom;

impl RandomSource for ThreadRandom {
    fn sample_unit(&mut self) -> f64 {
        rand::random()
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct EventPromptLoader;

impl EventPromptLoader {
    pub fn load_dir(&self, root: impl AsRef<Path>) -> Result<Vec<EventPrompt>, EventPromptError> {
        let root = root.as_ref();
        let mut markdown_files = Vec::new();
        let mut pending = vec![root.to_owned()];
        while let Some(directory) = pending.pop() {
            let entries = fs::read_dir(&directory).map_err(|source| EventPromptError::Io {
                path: directory.clone(),
                source,
            })?;
            for entry in entries {
                let entry = entry.map_err(|source| EventPromptError::Io {
                    path: directory.clone(),
                    source,
                })?;
                let path = entry.path();
                let file_type = entry.file_type().map_err(|source| EventPromptError::Io {
                    path: path.clone(),
                    source,
                })?;
                if file_type.is_dir() {
                    pending.push(path);
                } else if file_type.is_file()
                    && path.extension().is_some_and(|extension| extension == "md")
                {
                    markdown_files.push(path);
                }
            }
        }
        markdown_files.sort();
        if markdown_files.len() > MAX_EVENT_PROMPTS {
            return Err(EventPromptError::TooManyPrompts {
                found: markdown_files.len(),
                maximum: MAX_EVENT_PROMPTS,
            });
        }

        let mut prompts = Vec::with_capacity(markdown_files.len());
        let mut ids = BTreeMap::<String, PathBuf>::new();
        for path in markdown_files {
            let prompt = self.load_file(&path)?;
            if let Some(first) = ids.insert(prompt.metadata.id.clone(), path.clone()) {
                return Err(EventPromptError::DuplicateId {
                    id: prompt.metadata.id,
                    first,
                    second: path,
                });
            }
            prompts.push(prompt);
        }
        Ok(prompts)
    }

    pub fn load_file(&self, path: impl AsRef<Path>) -> Result<EventPrompt, EventPromptError> {
        let path = path.as_ref();
        let metadata = fs::metadata(path).map_err(|source| EventPromptError::Io {
            path: path.to_owned(),
            source,
        })?;
        if metadata.len() > MAX_EVENT_PROMPT_BYTES {
            return Err(EventPromptError::TooLarge {
                path: path.to_owned(),
                maximum_bytes: MAX_EVENT_PROMPT_BYTES,
            });
        }
        let file = fs::File::open(path).map_err(|source| EventPromptError::Io {
            path: path.to_owned(),
            source,
        })?;
        let mut content = String::new();
        file.take(MAX_EVENT_PROMPT_BYTES + 1)
            .read_to_string(&mut content)
            .map_err(|source| EventPromptError::Io {
                path: path.to_owned(),
                source,
            })?;
        if content.len() as u64 > MAX_EVENT_PROMPT_BYTES {
            return Err(EventPromptError::TooLarge {
                path: path.to_owned(),
                maximum_bytes: MAX_EVENT_PROMPT_BYTES,
            });
        }
        parse_prompt(path, &content)
    }

    pub fn select<'a>(
        &self,
        prompts: &'a [EventPrompt],
        context: &EventEligibility<'_>,
        random: &mut impl RandomSource,
    ) -> Result<Option<&'a EventPrompt>, EventPromptError> {
        let eligible = prompts
            .iter()
            .filter(|prompt| prompt.is_eligible(context))
            .collect::<Vec<_>>();
        if eligible.is_empty() {
            return Ok(None);
        }
        if eligible.len() > MAX_EVENT_PROMPTS {
            return Err(EventPromptError::TooManyPrompts {
                found: eligible.len(),
                maximum: MAX_EVENT_PROMPTS,
            });
        }
        for prompt in &eligible {
            validate_metadata(&prompt.source_path, &prompt.metadata)?;
        }

        let total_weight: f64 = eligible.iter().map(|prompt| prompt.metadata.weight).sum();
        if !total_weight.is_finite() || total_weight <= 0.0 {
            return Err(EventPromptError::InvalidTotalWeight);
        }
        let sample = random.sample_unit();
        if !sample.is_finite() || !(0.0..1.0).contains(&sample) {
            return Err(EventPromptError::InvalidRandomSample { sample });
        }
        let mut draw = sample * total_weight;
        for prompt in &eligible {
            if draw < prompt.metadata.weight {
                return Ok(Some(prompt));
            }
            draw -= prompt.metadata.weight;
        }

        // Floating-point rounding may leave an infinitesimal remainder.
        Ok(eligible.last().copied())
    }
}

fn parse_prompt(path: &Path, content: &str) -> Result<EventPrompt, EventPromptError> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut lines = content.lines();
    if lines.next().is_none_or(|line| line.trim() != "---") {
        return Err(EventPromptError::MissingFrontmatter {
            path: path.to_owned(),
        });
    }

    let mut frontmatter = Vec::new();
    let mut found_closing = false;
    for line in lines.by_ref() {
        if line.trim() == "---" {
            found_closing = true;
            break;
        }
        frontmatter.push(line);
    }
    if !found_closing {
        return Err(EventPromptError::MissingFrontmatter {
            path: path.to_owned(),
        });
    }

    let metadata: EventPromptMetadata =
        serde_json::from_str(&frontmatter.join("\n")).map_err(|source| {
            EventPromptError::InvalidFrontmatter {
                path: path.to_owned(),
                source,
            }
        })?;
    validate_metadata(path, &metadata)?;
    let prompt = lines.collect::<Vec<_>>().join("\n").trim().to_owned();
    if prompt.is_empty() {
        return Err(EventPromptError::InvalidMetadata {
            path: path.to_owned(),
            reason: "prompt body must not be empty".to_owned(),
        });
    }

    Ok(EventPrompt {
        metadata,
        prompt,
        source_path: path.to_owned(),
    })
}

fn validate_metadata(path: &Path, metadata: &EventPromptMetadata) -> Result<(), EventPromptError> {
    let invalid = |reason: String| EventPromptError::InvalidMetadata {
        path: path.to_owned(),
        reason,
    };
    if metadata.schema_version != 1 {
        return Err(invalid("schema_version must be 1".to_owned()));
    }
    if !valid_id(&metadata.id) {
        return Err(invalid(
            "id must use lowercase ASCII letters, digits, hyphens, or underscores".to_owned(),
        ));
    }
    if metadata.title.trim().is_empty() || metadata.title.chars().count() > MAX_EVENT_TITLE_CHARS {
        return Err(invalid(format!(
            "title must contain between 1 and {MAX_EVENT_TITLE_CHARS} characters"
        )));
    }
    if !metadata.weight.is_finite() || metadata.weight <= 0.0 || metadata.weight > MAX_EVENT_WEIGHT
    {
        return Err(invalid(format!(
            "weight must be finite, greater than zero, and at most {MAX_EVENT_WEIGHT}"
        )));
    }
    if !(1..=20).contains(&metadata.minimum_level) {
        return Err(invalid("minimum_level must be between 1 and 20".to_owned()));
    }
    if metadata
        .maximum_level
        .is_some_and(|maximum| !(metadata.minimum_level..=20).contains(&maximum))
    {
        return Err(invalid(
            "maximum_level must be between minimum_level and 20".to_owned(),
        ));
    }
    if metadata.cooldown_turns > MAX_EVENT_COOLDOWN_TURNS {
        return Err(invalid(format!(
            "cooldown_turns must not exceed {MAX_EVENT_COOLDOWN_TURNS}"
        )));
    }
    validate_labels("sensitivity_tags", &metadata.sensitivity_tags).map_err(invalid)?;
    validate_labels("participant_aliases", &metadata.participant_aliases).map_err(invalid)?;
    if metadata.enabled && metadata.sensitivity_tags.is_empty() {
        return Err(invalid(
            "enabled private events must declare at least one sensitivity tag".to_owned(),
        ));
    }
    if metadata.enabled && metadata.participant_aliases.is_empty() {
        return Err(invalid(
            "enabled private events must declare at least one consenting participant alias"
                .to_owned(),
        ));
    }
    if metadata.enabled && metadata.cooldown_turns == 0 {
        return Err(invalid(
            "enabled private events must declare a positive cooldown".to_owned(),
        ));
    }
    Ok(())
}

fn validate_labels(field: &str, labels: &[String]) -> Result<(), String> {
    if labels.len() > MAX_EVENT_LABELS {
        return Err(format!(
            "{field} may contain at most {MAX_EVENT_LABELS} entries"
        ));
    }
    let mut seen = BTreeSet::new();
    for label in labels {
        let normalized = normalize_label(label);
        if !is_valid_opaque_id(&normalized) {
            return Err(format!("{field} entries must be valid opaque identifiers"));
        }
        if !seen.insert(normalized) {
            return Err(format!("{field} must not contain duplicates"));
        }
    }
    Ok(())
}

fn valid_id(id: &str) -> bool {
    is_valid_opaque_id(id)
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_".contains(&byte))
        && id.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
}

fn normalize_label(label: &str) -> String {
    label.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::NamedTempFile;

    use super::*;

    struct FixedRandom(f64);

    impl RandomSource for FixedRandom {
        fn sample_unit(&mut self) -> f64 {
            self.0
        }
    }

    fn prompt(id: &str, weight: f64) -> EventPrompt {
        EventPrompt {
            metadata: EventPromptMetadata {
                schema_version: 1,
                id: id.to_owned(),
                title: id.to_owned(),
                weight,
                minimum_level: 1,
                maximum_level: None,
                cooldown_turns: 1,
                sensitivity_tags: vec!["general".to_owned()],
                participant_aliases: vec!["tester".to_owned()],
                enabled: true,
            },
            prompt: "Fictionalize this event".to_owned(),
            source_path: PathBuf::from(format!("{id}.md")),
        }
    }

    #[test]
    fn parses_json_frontmatter_and_markdown_body() {
        let mut file = NamedTempFile::new().expect("temporary file");
        write!(
            file,
            r#"---
{{
  "id": "tram-delay",
  "title": "The Clockwork Carriage Stops",
  "weight": 3,
  "minimum_level": 2,
  "cooldown_turns": 1,
  "sensitivity_tags": ["travel"],
  "participant_aliases": ["the_cartographer"],
  "enabled": true
}}
---

## Transformation

Turn the delay into a harmless magical obstruction.
"#
        )
        .expect("write prompt");

        let parsed = EventPromptLoader
            .load_file(file.path())
            .expect("prompt should parse");
        assert_eq!(parsed.metadata.id, "tram-delay");
        assert!(parsed.prompt.contains("magical obstruction"));
    }

    #[test]
    fn consent_sensitivity_level_and_cooldown_are_all_required() {
        let event = EventPrompt {
            metadata: EventPromptMetadata {
                sensitivity_tags: vec!["embarrassment".to_owned()],
                participant_aliases: vec!["friend-a".to_owned()],
                minimum_level: 3,
                cooldown_turns: 5,
                ..prompt("awkward-banquet", 1.0).metadata
            },
            ..prompt("awkward-banquet", 1.0)
        };
        let allowed = BTreeSet::from(["embarrassment".to_owned()]);
        let consented = BTreeSet::from(["friend-a".to_owned()]);
        let last = HashMap::from([("awkward-banquet".to_owned(), 7)]);
        let eligible = EventEligibility {
            inspiration_enabled: true,
            party_level: 3,
            current_turn: 12,
            allowed_sensitivity_tags: &allowed,
            consenting_participant_aliases: &consented,
            last_triggered_turn: &last,
        };

        assert!(event.is_eligible(&eligible));
        let denied = EventEligibility {
            consenting_participant_aliases: &BTreeSet::new(),
            ..eligible
        };
        assert!(!event.is_eligible(&denied));
    }

    #[test]
    fn weighted_selection_uses_injected_randomness() {
        let prompts = vec![prompt("small", 1.0), prompt("large", 3.0)];
        let allowed = BTreeSet::from(["general".to_owned()]);
        let consented = BTreeSet::from(["tester".to_owned()]);
        let last = HashMap::new();
        let eligibility = EventEligibility {
            inspiration_enabled: true,
            party_level: 1,
            current_turn: 0,
            allowed_sensitivity_tags: &allowed,
            consenting_participant_aliases: &consented,
            last_triggered_turn: &last,
        };

        let selected = EventPromptLoader
            .select(&prompts, &eligibility, &mut FixedRandom(0.5))
            .expect("selection should succeed")
            .expect("one event should be selected");
        assert_eq!(selected.metadata.id, "large");
    }

    #[test]
    fn private_event_requires_explicit_enablement() {
        let mut file = NamedTempFile::new().expect("temporary file");
        write!(
            file,
            r#"---
{{
  "id": "quiet-memory",
  "title": "A Quiet Memory",
  "sensitivity_tags": ["general"],
  "participant_aliases": ["tester"]
}}
---

Fictionalize this memory.
"#
        )
        .expect("write prompt");

        let parsed = EventPromptLoader
            .load_file(file.path())
            .expect("disabled prompt should still parse");
        assert!(!parsed.metadata.enabled);
    }

    #[test]
    fn rejects_weight_totals_that_cannot_be_sampled_safely() {
        let prompts = vec![prompt("first", f64::MAX), prompt("second", f64::MAX)];
        let allowed = BTreeSet::from(["general".to_owned()]);
        let consented = BTreeSet::from(["tester".to_owned()]);
        let last = HashMap::new();
        let eligibility = EventEligibility {
            inspiration_enabled: true,
            party_level: 1,
            current_turn: 0,
            allowed_sensitivity_tags: &allowed,
            consenting_participant_aliases: &consented,
            last_triggered_turn: &last,
        };

        assert!(matches!(
            EventPromptLoader.select(&prompts, &eligibility, &mut FixedRandom(0.5)),
            Err(EventPromptError::InvalidMetadata { .. })
        ));
    }

    #[test]
    fn campaign_level_opt_in_is_required() {
        let event = prompt("private-memory", 1.0);
        let allowed = BTreeSet::from(["general".to_owned()]);
        let consented = BTreeSet::from(["tester".to_owned()]);
        let last = HashMap::new();
        let eligibility = EventEligibility {
            inspiration_enabled: false,
            party_level: 1,
            current_turn: 0,
            allowed_sensitivity_tags: &allowed,
            consenting_participant_aliases: &consented,
            last_triggered_turn: &last,
        };

        assert!(!event.is_eligible(&eligibility));
    }
}
