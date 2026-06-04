use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::{DirectoryTypeDefinition, DirectoryTypeRule, DirectoryTypeSignal, Settings};
use crate::db::DirectoryClassification;
use crate::index::{IndexError, Result};

const BUILTIN_DEFINITIONS: &[(&str, &str)] = &[
    (
        "chrome_extension.json",
        include_str!("directory_types/chrome_extension.json"),
    ),
    ("rust.json", include_str!("directory_types/rust.json")),
    ("node.json", include_str!("directory_types/node.json")),
    ("next.json", include_str!("directory_types/next.json")),
    ("python.json", include_str!("directory_types/python.json")),
    ("rails.json", include_str!("directory_types/rails.json")),
    (
        "migrations.json",
        include_str!("directory_types/migrations.json"),
    ),
];

pub fn classify_directory(
    directory: &Path,
    settings: &Settings,
) -> Result<Vec<DirectoryClassification>> {
    let definitions = load_definitions(settings)?;
    let detected_unix_seconds = unix_seconds(SystemTime::now());
    let directory_path = path_to_string(directory);
    let mut classifications = Vec::new();

    for definition in definitions {
        for rule in definition.rules {
            let Some(evidence) = matches_rule(directory, &rule) else {
                continue;
            };

            classifications.push(DirectoryClassification {
                directory_path: directory_path.clone(),
                label: definition.label.clone(),
                confidence: rule.confidence,
                detector: rule.id,
                evidence_path: evidence.evidence_path.map(|path| path_to_string(&path)),
                evidence_summary: rule
                    .evidence_summary
                    .unwrap_or_else(|| evidence.summaries.join("; ")),
                detected_unix_seconds,
            });
        }
    }

    classifications.sort_by(|left, right| {
        left.label
            .cmp(&right.label)
            .then_with(|| right.confidence.total_cmp(&left.confidence))
            .then_with(|| left.detector.cmp(&right.detector))
    });
    classifications.dedup_by(|left, right| left.label == right.label);

    Ok(classifications)
}

fn load_definitions(settings: &Settings) -> Result<Vec<DirectoryTypeDefinition>> {
    let mut definitions = Vec::new();

    for (name, text) in BUILTIN_DEFINITIONS {
        definitions.push(parse_definition(Path::new(name), text)?);
    }

    definitions.extend(settings.detectors.clone());

    Ok(definitions)
}

fn parse_definition(path: &Path, text: &str) -> Result<DirectoryTypeDefinition> {
    serde_json::from_str(text).map_err(|source| IndexError::ParseDirectoryTypeDefinition {
        path: path.to_path_buf(),
        source,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuleEvidence {
    evidence_path: Option<PathBuf>,
    summaries: Vec<String>,
}

fn matches_rule(directory: &Path, rule: &DirectoryTypeRule) -> Option<RuleEvidence> {
    let mut evidence_path = None;
    let mut summaries = Vec::new();

    for signal in &rule.signals {
        let signal_match = matches_signal(directory, signal)?;
        if evidence_path.is_none() {
            evidence_path = signal_match.evidence_path;
        }
        summaries.push(signal_match.summary);
    }

    Some(RuleEvidence {
        evidence_path,
        summaries,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SignalMatch {
    evidence_path: Option<PathBuf>,
    summary: String,
}

fn matches_signal(directory: &Path, signal: &DirectoryTypeSignal) -> Option<SignalMatch> {
    match signal {
        DirectoryTypeSignal::FileExists { path } => {
            let path = directory.join(path);
            path.exists().then(|| SignalMatch {
                evidence_path: Some(path),
                summary: "required file exists".to_string(),
            })
        }
        DirectoryTypeSignal::FileContains {
            path,
            contains_any,
            contains_all,
        } => {
            let path = directory.join(path);
            let text = read_small_text(&path)?.to_ascii_lowercase();
            let any_matches = contains_any.is_empty()
                || contains_any
                    .iter()
                    .any(|needle| text.contains(&needle.to_ascii_lowercase()));
            let all_match = contains_all
                .iter()
                .all(|needle| text.contains(&needle.to_ascii_lowercase()));

            (any_matches && all_match).then(|| SignalMatch {
                evidence_path: Some(path),
                summary: "file contains required text".to_string(),
            })
        }
        DirectoryTypeSignal::DirectoryName {
            equals_any,
            contains_any,
        } => {
            let name = directory
                .file_name()
                .map(|name| name.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            let equals = !equals_any.is_empty()
                && equals_any
                    .iter()
                    .any(|candidate| name == candidate.to_ascii_lowercase());
            let contains = contains_any
                .iter()
                .any(|candidate| name.contains(&candidate.to_ascii_lowercase()));

            (equals || contains).then(|| SignalMatch {
                evidence_path: None,
                summary: format!("directory name matched {name}"),
            })
        }
        DirectoryTypeSignal::ChildName {
            contains_any,
            starts_with_any,
            ends_with_any,
        } => {
            let entries = fs::read_dir(directory).ok()?;
            for entry in entries.filter_map(std::result::Result::ok) {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                let contains = contains_any
                    .iter()
                    .any(|candidate| name.contains(&candidate.to_ascii_lowercase()));
                let starts = starts_with_any
                    .iter()
                    .any(|candidate| name.starts_with(&candidate.to_ascii_lowercase()));
                let ends = ends_with_any
                    .iter()
                    .any(|candidate| name.ends_with(&candidate.to_ascii_lowercase()));

                if contains || starts || ends {
                    return Some(SignalMatch {
                        evidence_path: Some(entry.path()),
                        summary: format!("child name matched {name}"),
                    });
                }
            }

            None
        }
    }
}

fn read_small_text(path: &Path) -> Option<String> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > 131_072 {
        return None;
    }

    let bytes = fs::read(path).ok()?;
    if bytes.contains(&0) {
        return None;
    }

    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn unix_seconds(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_chrome_extension_from_builtin_json() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("manifest.json"),
            r#"{"manifest_version":3,"action":{},"permissions":["storage"]}"#,
        )
        .unwrap();

        let labels = classify_directory(temp.path(), &Settings::default())
            .unwrap()
            .into_iter()
            .map(|classification| classification.label)
            .collect::<Vec<_>>();

        assert!(labels.contains(&"chrome extension".to_string()));
    }

    #[test]
    fn detects_custom_directory_type_from_config_array() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("custom.marker"), "yes").unwrap();

        let settings = Settings {
            detectors: serde_json::from_str(
                r#"
[
{
  "label": "custom project",
  "rules": [
    {
      "id": "marker",
      "confidence": 0.99,
      "signals": [
        { "kind": "file_exists", "path": "custom.marker" }
      ]
    }
  ]
}
]
"#,
            )
            .unwrap(),
            ..Settings::default()
        };

        let labels = classify_directory(&project, &settings)
            .unwrap()
            .into_iter()
            .map(|classification| classification.label)
            .collect::<Vec<_>>();

        assert!(labels.contains(&"custom project".to_string()));
    }
}
