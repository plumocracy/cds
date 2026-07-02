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
    ("go.json", include_str!("directory_types/go.json")),
    (
        "terraform.json",
        include_str!("directory_types/terraform.json"),
    ),
    ("docker.json", include_str!("directory_types/docker.json")),
    (
        "kubernetes.json",
        include_str!("directory_types/kubernetes.json"),
    ),
    (
        "monorepo.json",
        include_str!("directory_types/monorepo.json"),
    ),
    ("vite.json", include_str!("directory_types/vite.json")),
    ("react.json", include_str!("directory_types/react.json")),
    (
        "sveltekit.json",
        include_str!("directory_types/sveltekit.json"),
    ),
    ("astro.json", include_str!("directory_types/astro.json")),
    ("ios.json", include_str!("directory_types/ios.json")),
    ("android.json", include_str!("directory_types/android.json")),
    ("java.json", include_str!("directory_types/java.json")),
    ("dotnet.json", include_str!("directory_types/dotnet.json")),
    ("laravel.json", include_str!("directory_types/laravel.json")),
    ("django.json", include_str!("directory_types/django.json")),
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
        DirectoryTypeSignal::ChildFileContains {
            name_contains_any,
            name_starts_with_any,
            name_ends_with_any,
            contains_any,
            contains_all,
        } => {
            let entries = fs::read_dir(directory).ok()?;
            for entry in entries.filter_map(std::result::Result::ok) {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if !child_name_matches(
                    &name,
                    name_contains_any,
                    name_starts_with_any,
                    name_ends_with_any,
                ) {
                    continue;
                }

                let Some(text) =
                    read_small_text(&entry.path()).map(|text| text.to_ascii_lowercase())
                else {
                    continue;
                };
                let any_matches = contains_any.is_empty()
                    || contains_any
                        .iter()
                        .any(|needle| text.contains(&needle.to_ascii_lowercase()));
                let all_match = contains_all
                    .iter()
                    .all(|needle| text.contains(&needle.to_ascii_lowercase()));

                if any_matches && all_match {
                    return Some(SignalMatch {
                        evidence_path: Some(entry.path()),
                        summary: format!("child file {name} contains required text"),
                    });
                }
            }

            None
        }
    }
}

fn child_name_matches(
    name: &str,
    contains_any: &[String],
    starts_with_any: &[String],
    ends_with_any: &[String],
) -> bool {
    let has_filters =
        !contains_any.is_empty() || !starts_with_any.is_empty() || !ends_with_any.is_empty();
    if !has_filters {
        return true;
    }

    contains_any
        .iter()
        .any(|candidate| name.contains(&candidate.to_ascii_lowercase()))
        || starts_with_any
            .iter()
            .any(|candidate| name.starts_with(&candidate.to_ascii_lowercase()))
        || ends_with_any
            .iter()
            .any(|candidate| name.ends_with(&candidate.to_ascii_lowercase()))
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

    fn labels_for(directory: &Path) -> Vec<String> {
        classify_directory(directory, &Settings::default())
            .unwrap()
            .into_iter()
            .map(|classification| classification.label)
            .collect()
    }

    #[test]
    fn detects_chrome_extension_from_builtin_json() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("manifest.json"),
            r#"{"manifest_version":3,"action":{},"permissions":["storage"]}"#,
        )
        .unwrap();

        let labels = labels_for(temp.path());

        assert!(labels.contains(&"chrome extension".to_string()));
    }

    #[test]
    fn detects_common_project_types_from_builtin_json() {
        let temp = tempfile::tempdir().unwrap();

        let go = temp.path().join("go");
        fs::create_dir_all(&go).unwrap();
        fs::write(go.join("go.mod"), "module example.com/app").unwrap();
        assert!(labels_for(&go).contains(&"go project".to_string()));

        let terraform = temp.path().join("terraform");
        fs::create_dir_all(&terraform).unwrap();
        fs::write(
            terraform.join("main.tf"),
            "resource \"null_resource\" \"demo\" {}",
        )
        .unwrap();
        assert!(labels_for(&terraform).contains(&"terraform project".to_string()));

        let docker = temp.path().join("docker");
        fs::create_dir_all(&docker).unwrap();
        fs::write(docker.join("Dockerfile"), "FROM alpine").unwrap();
        assert!(labels_for(&docker).contains(&"dockerized app".to_string()));

        let kubernetes = temp.path().join("kubernetes");
        fs::create_dir_all(&kubernetes).unwrap();
        fs::write(
            kubernetes.join("deployment.yaml"),
            "apiVersion: apps/v1\nkind: Deployment\n",
        )
        .unwrap();
        assert!(labels_for(&kubernetes).contains(&"kubernetes config".to_string()));

        let monorepo = temp.path().join("monorepo");
        fs::create_dir_all(&monorepo).unwrap();
        fs::write(
            monorepo.join("pnpm-workspace.yaml"),
            "packages:\n  - apps/*\n",
        )
        .unwrap();
        assert!(labels_for(&monorepo).contains(&"monorepo".to_string()));

        let vite = temp.path().join("vite");
        fs::create_dir_all(&vite).unwrap();
        fs::write(vite.join("vite.config.ts"), "export default {}").unwrap();
        assert!(labels_for(&vite).contains(&"vite app".to_string()));

        let react = temp.path().join("react");
        fs::create_dir_all(&react).unwrap();
        fs::write(
            react.join("package.json"),
            r#"{"dependencies":{"react":"latest","vite":"latest"}}"#,
        )
        .unwrap();
        assert!(labels_for(&react).contains(&"react app".to_string()));

        let sveltekit = temp.path().join("sveltekit");
        fs::create_dir_all(&sveltekit).unwrap();
        fs::write(sveltekit.join("svelte.config.js"), "export default {}").unwrap();
        assert!(labels_for(&sveltekit).contains(&"sveltekit app".to_string()));

        let astro = temp.path().join("astro");
        fs::create_dir_all(&astro).unwrap();
        fs::write(astro.join("astro.config.mjs"), "export default {}").unwrap();
        assert!(labels_for(&astro).contains(&"astro site".to_string()));

        let ios = temp.path().join("ios");
        fs::create_dir_all(ios.join("App.xcodeproj")).unwrap();
        assert!(labels_for(&ios).contains(&"ios app".to_string()));

        let android = temp.path().join("android");
        fs::create_dir_all(&android).unwrap();
        fs::write(
            android.join("build.gradle"),
            "plugins { id 'com.android.application' }\nandroid { }",
        )
        .unwrap();
        assert!(labels_for(&android).contains(&"android app".to_string()));

        let java = temp.path().join("java");
        fs::create_dir_all(&java).unwrap();
        fs::write(java.join("pom.xml"), "<project></project>").unwrap();
        assert!(labels_for(&java).contains(&"java project".to_string()));

        let dotnet = temp.path().join("dotnet");
        fs::create_dir_all(&dotnet).unwrap();
        fs::write(dotnet.join("App.csproj"), "<Project></Project>").unwrap();
        assert!(labels_for(&dotnet).contains(&".net project".to_string()));

        let laravel = temp.path().join("laravel");
        fs::create_dir_all(&laravel).unwrap();
        fs::write(laravel.join("artisan"), "#!/usr/bin/env php").unwrap();
        fs::write(
            laravel.join("composer.json"),
            r#"{"require":{"laravel/framework":"^11.0"}}"#,
        )
        .unwrap();
        assert!(labels_for(&laravel).contains(&"laravel app".to_string()));

        let django = temp.path().join("django");
        fs::create_dir_all(&django).unwrap();
        fs::write(
            django.join("manage.py"),
            "from django.core.management import execute_from_command_line",
        )
        .unwrap();
        assert!(labels_for(&django).contains(&"django app".to_string()));
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
