//! Resource-bounded inspector for canonical ESBT artifacts.

use esbt::anchor::AnchorTarget;
use esbt::clock::Version;
use esbt::wire::{Artifact, ArtifactKind, WIRE_FORMAT_VERSION, WIRE_HEADER_BYTES};
use esbt::ResourceLimits;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::process::ExitCode;

const DEFAULT_MAX_BYTES: usize = 64 * 1024 * 1024;
const INSPECTOR_MAX_ITEMS: usize = 2_000_000;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut path = None;
    let mut structural = false;
    let mut max_bytes = DEFAULT_MAX_BYTES;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--structural" => structural = true,
            "--max-bytes" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| "--max-bytes requires an integer".to_owned())?;
                max_bytes = value
                    .parse::<usize>()
                    .map_err(|_| "--max-bytes is not a platform-sized integer".to_owned())?;
                if max_bytes < WIRE_HEADER_BYTES {
                    return Err(format!("--max-bytes must be at least {WIRE_HEADER_BYTES}"));
                }
            }
            "-h" | "--help" => {
                println!(
                    "usage: esbt-inspect [--structural] [--max-bytes N] [FILE|-]\n\
                     validates one canonical ESBT artifact and prints one JSON summary"
                );
                return Ok(());
            }
            "-" => {
                if path.replace(argument).is_some() {
                    return Err("only one input is accepted".to_owned());
                }
            }
            _ if argument.starts_with('-') => {
                return Err(format!("unknown option {argument}"));
            }
            _ => {
                if path.replace(argument).is_some() {
                    return Err("only one input is accepted".to_owned());
                }
            }
        }
    }
    let path = path.unwrap_or_else(|| "-".to_owned());
    let bytes = read_bounded(&path, max_bytes)?;

    if structural {
        let kind = Artifact::classify(&bytes).map_err(|error| error_json(&path, &error))?;
        println!(
            "{{\"ok\":true,\"path\":{},\"wireVersion\":{},\"kind\":\"{}\",\"bytes\":{},\"payloadBytes\":{},\"semanticValidation\":false}}",
            json_string(&path),
            WIRE_FORMAT_VERSION,
            kind_name(kind),
            bytes.len(),
            bytes.len().saturating_sub(WIRE_HEADER_BYTES),
        );
        return Ok(());
    }

    let limits = ResourceLimits {
        max_message_bytes: max_bytes,
        max_operations_per_update: INSPECTOR_MAX_ITEMS,
        max_snapshot_items: INSPECTOR_MAX_ITEMS,
        max_pending_operations: INSPECTOR_MAX_ITEMS,
        max_document_units: INSPECTOR_MAX_ITEMS,
        max_retained_operations: INSPECTOR_MAX_ITEMS,
        ..ResourceLimits::default()
    };
    let artifact =
        Artifact::decode_with_limits(&bytes, &limits).map_err(|error| error_json(&path, &error))?;
    println!(
        "{{\"ok\":true,\"path\":{},\"wireVersion\":{},\"kind\":\"{}\",\"bytes\":{},\"payloadBytes\":{},\"semanticValidation\":true,\"summary\":{}}}",
        json_string(&path),
        WIRE_FORMAT_VERSION,
        kind_name(artifact.kind()),
        bytes.len(),
        bytes.len().saturating_sub(WIRE_HEADER_BYTES),
        summary(&artifact),
    );
    Ok(())
}

fn read_bounded(path: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    if path == "-" {
        io::stdin()
            .take(max_bytes.saturating_add(1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("could not read stdin: {error}"))?;
    } else {
        let metadata = Path::new(path)
            .metadata()
            .map_err(|error| format!("could not inspect {path}: {error}"))?;
        if !metadata.is_file() || metadata.len() > max_bytes as u64 {
            return Err(format!(
                "{path} is not a regular file within the {max_bytes}-byte limit"
            ));
        }
        File::open(path)
            .and_then(|file| {
                file.take(max_bytes.saturating_add(1) as u64)
                    .read_to_end(&mut bytes)
            })
            .map_err(|error| format!("could not read {path}: {error}"))?;
    }
    if bytes.len() > max_bytes {
        return Err(format!("input exceeds the {max_bytes}-byte limit"));
    }
    Ok(bytes)
}

fn summary(artifact: &Artifact) -> String {
    match artifact {
        Artifact::Update(update) => format!("{{\"operations\":{}}}", update.len()),
        Artifact::CompactSnapshot(snapshot) => format!(
            "{{\"liveAtoms\":{},\"deferredDeletes\":{},\"versionSites\":{},\"insertionSites\":{}}}",
            snapshot.atoms.len(),
            snapshot.delete_log.len(),
            site_count(&snapshot.version),
            site_count(&snapshot.insertions),
        ),
        Artifact::FullSnapshot(snapshot) => format!(
            "{{\"liveAtoms\":{},\"deferredDeletes\":{},\"versionSites\":{},\"historyFloorSites\":{},\"retainedOperations\":{},\"pendingOperations\":{}}}",
            snapshot.state.atoms.len(),
            snapshot.state.delete_log.len(),
            site_count(&snapshot.state.version),
            site_count(&snapshot.history_floor),
            snapshot.retained_operations().len(),
            snapshot.pending_operations.len(),
        ),
        Artifact::Version(version) => {
            let sites = site_count(version);
            let sparse: usize = version.receipts().map(|(_, _, seen)| seen.len()).sum();
            format!("{{\"sites\":{sites},\"sparseReceipts\":{sparse}}}")
        }
        Artifact::Anchor(anchor) => format!(
            "{{\"target\":\"{}\",\"affinity\":\"{}\"}}",
            anchor_target(&anchor.target),
            format!("{:?}", anchor.affinity).to_ascii_lowercase(),
        ),
        Artifact::CausalPosition(position) => format!(
            "{{\"versionSites\":{},\"target\":\"{}\",\"affinity\":\"{}\"}}",
            site_count(&position.version),
            anchor_target(&position.anchor.target),
            format!("{:?}", position.anchor.affinity).to_ascii_lowercase(),
        ),
    }
}

fn site_count(version: &Version) -> usize {
    version.receipts().count()
}

fn anchor_target(target: &AnchorTarget) -> &'static str {
    match target {
        AnchorTarget::Start => "start",
        AnchorTarget::End => "end",
        AnchorTarget::Item { .. } => "item",
    }
}

fn kind_name(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Update => "update",
        ArtifactKind::CompactSnapshot => "compact-snapshot",
        ArtifactKind::FullSnapshot => "full-snapshot",
        ArtifactKind::Version => "version",
        ArtifactKind::Anchor => "anchor",
        ArtifactKind::CausalPosition => "causal-position",
    }
}

fn error_json(path: &str, error: &esbt::EngineError) -> String {
    format!(
        "{{\"ok\":false,\"path\":{},\"code\":{},\"name\":\"{:?}\",\"message\":{}}}",
        json_string(path),
        error.code as u32,
        error.code,
        json_string(error.detail),
    )
}

fn json_string(value: &str) -> String {
    let mut encoded = String::from("\"");
    for character in value.chars() {
        match character {
            '\"' => encoded.push_str("\\\""),
            '\\' => encoded.push_str("\\\\"),
            '\n' => encoded.push_str("\\n"),
            '\r' => encoded.push_str("\\r"),
            '\t' => encoded.push_str("\\t"),
            value if value.is_control() => encoded.push_str(&format!("\\u{:04x}", value as u32)),
            value => encoded.push(value),
        }
    }
    encoded.push('\"');
    encoded
}
