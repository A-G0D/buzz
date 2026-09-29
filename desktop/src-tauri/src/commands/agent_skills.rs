//! Local Agent Skills library for the shared Buzz workspace.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Cursor, Read, Write},
    path::{Component, Path, PathBuf},
};
use tauri::AppHandle;

use crate::{
    commands::export_util::save_bytes_with_dialog,
    managed_agents::{nest_dir, validate_visible_text},
};

const MAX_SKILL_BYTES: usize = 256 * 1024;
const MAX_SKILL_PACK_BYTES: usize = 1024 * 1024;
const MAX_SKILL_PACK_EXPANDED_BYTES: usize = 1024 * 1024;
const MAX_SKILL_PACK_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_SKILLS_PER_PACK: usize = 32;
const MANAGED_SKILL: &str = "buzz-cli";
const SKILL_PACK_FORMAT: &str = "buzz-agent-skill-pack";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSkillSummary {
    pub name: String,
    pub description: String,
    pub content_hash: String,
    pub validation_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSkillDetails {
    pub name: String,
    pub description: String,
    pub content: String,
    pub content_hash: String,
    pub validation_error: Option<String>,
    pub runtime_compatibility: Vec<AgentSkillRuntimeCompatibility>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSkillRuntimeCompatibility {
    pub runtime_id: String,
    pub runtime_label: String,
    pub skill_directory: String,
    pub status: SkillRuntimeLinkStatus,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SkillRuntimeLinkStatus {
    Linked,
    Missing,
    Conflict,
    Blocked,
}

fn skills_root() -> Result<PathBuf, String> {
    let root = nest_dir().ok_or("cannot resolve Buzz workspace")?;
    ensure_real_directory(&root, Path::new(".agents/skills"), false)
}

fn ensure_real_directory(root: &Path, relative: &Path, create: bool) -> Result<PathBuf, String> {
    if root
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(format!(
            "Refusing to use a symlink as the Buzz workspace: {}",
            root.display()
        ));
    }
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return Err("Invalid skill directory path".to_string());
        };
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "Refusing to follow a symlink at {}",
                    current.display()
                ));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(format!("Expected a directory at {}", current.display()));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
                fs::create_dir(&current)
                    .map_err(|e| format!("Create {}: {e}", current.display()))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(current);
            }
            Err(error) => return Err(format!("Inspect {}: {error}", current.display())),
        }
    }
    Ok(current)
}

fn lock_skill_writes(root: &Path) -> Result<fs::File, String> {
    let agents_dir = ensure_real_directory(root, Path::new(".agents"), true)?;
    let lock_path = agents_dir.join(".skills-write.lock");
    if fs::symlink_metadata(&lock_path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err("Refusing to use a symlink as the skill write lock.".to_string());
    }
    let mut options = fs::OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        options.mode(0o600);
    }
    let lock = options
        .open(&lock_path)
        .map_err(|error| format!("Open skill write lock: {error}"))?;
    if !lock
        .metadata()
        .map_err(|error| format!("Inspect skill write lock: {error}"))?
        .is_file()
    {
        return Err("Skill write lock must be a regular file.".to_string());
    }
    lock.lock()
        .map_err(|error| format!("Lock skill workspace for writing: {error}"))?;
    Ok(lock)
}

fn validate_name(name: &str) -> Result<(), String> {
    let characters: Vec<char> = name.chars().collect();
    let is_lowercase_alphanumeric = |character: char| {
        character.is_numeric() || (character.is_alphabetic() && character.is_lowercase())
    };
    let valid = !characters.is_empty()
        && characters.len() <= 64
        && name.len() <= 255
        && characters
            .first()
            .is_some_and(|character| is_lowercase_alphanumeric(*character))
        && characters
            .iter()
            .all(|character| *character == '-' || is_lowercase_alphanumeric(*character))
        && characters.last() != Some(&'-')
        && !name.contains("--");
    if !valid {
        return Err(
            "Skill names must be 1–64 lowercase alphanumeric characters or single hyphens, start with a letter or number, and fit within a 255-byte path component."
                .to_string(),
        );
    }
    if name == MANAGED_SKILL {
        return Err(
            "The built-in buzz-cli skill is managed by Buzz and cannot be edited here.".to_string(),
        );
    }
    Ok(())
}

fn optional_frontmatter_string<'a>(
    frontmatter: &'a serde_yaml::Value,
    field: &str,
) -> Result<Option<&'a str>, String> {
    match frontmatter.get(field) {
        None => Ok(None),
        Some(serde_yaml::Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(format!(
            "Optional frontmatter field '{field}' must be a string."
        )),
    }
}

fn validate_content(expected_name: &str, content: &str) -> Result<String, String> {
    if content.len() > MAX_SKILL_BYTES {
        return Err(format!(
            "SKILL.md must be smaller than {} KiB.",
            MAX_SKILL_BYTES / 1024
        ));
    }
    for raw_line in content.split_inclusive('\n') {
        let line = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        validate_visible_text(line, "Skill instructions", true)?;
    }

    let mut lines = content.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return Err(
            "SKILL.md must start with YAML frontmatter delimited by --- lines.".to_string(),
        );
    }
    let mut frontmatter_lines = Vec::new();
    let mut closed = false;
    for line in lines.by_ref() {
        if line.trim_end() == "---" {
            closed = true;
            break;
        }
        frontmatter_lines.push(line);
    }
    if !closed {
        return Err("SKILL.md is missing the closing --- frontmatter delimiter.".to_string());
    }
    let yaml = frontmatter_lines.join("\n");
    let frontmatter: serde_yaml::Value =
        serde_yaml::from_str(&yaml).map_err(|e| format!("Invalid YAML frontmatter: {e}"))?;
    let name = frontmatter
        .get("name")
        .and_then(serde_yaml::Value::as_str)
        .ok_or("Frontmatter must contain a string name.")?;
    validate_name(name)?;
    if name != expected_name {
        return Err(format!(
            "Frontmatter name '{name}' must match the skill folder name '{expected_name}'."
        ));
    }
    let description = frontmatter
        .get("description")
        .and_then(serde_yaml::Value::as_str)
        .ok_or("Frontmatter must contain a string description.")?;
    let description_chars = description.chars().count();
    if description.trim().is_empty() || description_chars > 1024 {
        return Err("Description must contain 1–1024 characters.".to_string());
    }
    validate_visible_text(description, "Skill description", false)?;

    if let Some(license) = optional_frontmatter_string(&frontmatter, "license")? {
        if license.trim().is_empty() {
            return Err("License must not be empty when provided.".to_string());
        }
        validate_visible_text(license, "Skill license", false)?;
    }
    if let Some(compatibility) = optional_frontmatter_string(&frontmatter, "compatibility")? {
        let length = compatibility.chars().count();
        if compatibility.trim().is_empty() || length > 500 {
            return Err("Compatibility must contain 1–500 characters when provided.".to_string());
        }
        validate_visible_text(compatibility, "Skill compatibility", false)?;
    }
    if let Some(metadata) = frontmatter.get("metadata") {
        let entries = metadata
            .as_mapping()
            .ok_or("Metadata must be a map of string keys to string values.")?;
        for (key, value) in entries {
            let key = key.as_str().ok_or("Metadata keys must be strings.")?;
            let value = value.as_str().ok_or("Metadata values must be strings.")?;
            validate_visible_text(key, "Skill metadata key", false)?;
            validate_visible_text(value, "Skill metadata value", true)?;
        }
    }
    if let Some(allowed_tools) = optional_frontmatter_string(&frontmatter, "allowed-tools")? {
        validate_visible_text(allowed_tools, "Allowed tools", false)?;
    }
    Ok(description.to_string())
}

fn content_hash(content: &str) -> String {
    hex::encode(Sha256::digest(content.as_bytes()))
}

fn skill_license(content: &str) -> Option<String> {
    let mut lines = content.lines();
    lines.next()?;
    let frontmatter = lines
        .take_while(|line| line.trim_end() != "---")
        .collect::<Vec<_>>()
        .join("\n");
    serde_yaml::from_str::<serde_yaml::Value>(&frontmatter)
        .ok()?
        .get("license")?
        .as_str()
        .map(str::to_string)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SkillPackManifest {
    format: &'static str,
    version: u8,
    exported_from: &'static str,
    skills: Vec<SkillPackEntry>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SkillPackEntry {
    name: String,
    description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    license: Option<String>,
    file: String,
    sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ParsedSkillPackManifest {
    format: String,
    version: u8,
    exported_from: String,
    skills: Vec<ParsedSkillPackEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ParsedSkillPackEntry {
    name: String,
    description: String,
    #[serde(default)]
    license: Option<String>,
    file: String,
    sha256: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSkillPackSkillPreview {
    pub name: String,
    pub description: String,
    pub license: Option<String>,
    pub content: String,
    pub content_hash: String,
    pub already_installed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSkillPackPreview {
    pub version: u8,
    pub exported_from: String,
    pub skills: Vec<AgentSkillPackSkillPreview>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewedAgentSkill {
    pub name: String,
    pub content_hash: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentSkillPackExportRequest {
    pub name: String,
    pub expected_content_hash: String,
}

#[derive(Debug, Clone)]
struct ParsedSkill {
    name: String,
    description: String,
    license: Option<String>,
    content: String,
    content_hash: String,
}

#[derive(Debug)]
struct ParsedSkillPack {
    version: u8,
    skills: Vec<ParsedSkill>,
    exported_from: String,
}

fn read_entry_bounded<R: Read>(
    mut entry: R,
    declared_size: u64,
    cap: usize,
    expanded: &mut usize,
) -> Result<Vec<u8>, String> {
    let declared = usize::try_from(declared_size).unwrap_or(usize::MAX);
    let remaining = MAX_SKILL_PACK_EXPANDED_BYTES.saturating_sub(*expanded);
    let read_cap = cap.min(remaining);
    if declared > read_cap {
        return Err("Agent skill pack exceeds its expanded-size limit.".into());
    }
    let mut contents = Vec::with_capacity(declared);
    entry
        .by_ref()
        .take(read_cap.saturating_add(1) as u64)
        .read_to_end(&mut contents)
        .map_err(|error| format!("Read agent skill pack entry: {error}"))?;
    if contents.len() > read_cap {
        return Err("Agent skill pack exceeds its expanded-size limit.".into());
    }
    *expanded += contents.len();
    Ok(contents)
}

fn skill_name_key(name: &str) -> String {
    name.to_lowercase()
}

fn validate_pack_skill_names(names: &[String]) -> Result<(), String> {
    if names.is_empty() || names.len() > MAX_SKILLS_PER_PACK {
        return Err(format!(
            "Agent skill packs must contain 1–{MAX_SKILLS_PER_PACK} skills."
        ));
    }
    let mut seen = BTreeSet::new();
    for name in names {
        validate_name(name)?;
        if !seen.insert(skill_name_key(name)) {
            return Err(
                "Agent skill pack contains duplicate or case-insensitive skill names.".into(),
            );
        }
    }
    Ok(())
}

fn parse_skill_pack(bytes: &[u8]) -> Result<ParsedSkillPack, String> {
    if bytes.len() > MAX_SKILL_PACK_BYTES {
        return Err("Agent skill packs must be at most 1 MiB.".to_string());
    }
    if bytes.len() < 22 {
        return Err("This file is not a complete agent skill pack.".to_string());
    }

    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| format!("Read agent skill pack: {error}"))?;
    if !(2..=MAX_SKILLS_PER_PACK + 1).contains(&archive.len()) {
        return Err(format!(
            "A skill pack must contain a manifest and 1–{MAX_SKILLS_PER_PACK} SKILL.md files."
        ));
    }

    let mut manifest_bytes = None;
    let mut skill_files = BTreeMap::new();
    let mut entry_names = BTreeSet::new();
    let mut expanded = 0usize;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|error| format!("Read agent skill pack entry: {error}"))?;
        let entry_name = std::str::from_utf8(entry.name_raw())
            .map_err(|_| "Agent skill pack contains a non-UTF-8 file name.".to_string())?
            .to_string();
        if !entry_names.insert(entry_name.clone()) {
            return Err("Agent skill pack contains duplicate file names.".to_string());
        }
        if entry.is_dir() || entry.enclosed_name().is_none() {
            return Err("Agent skill pack contains an unsafe or unexpected path.".to_string());
        }
        if let Some(mode) = entry.unix_mode() {
            let file_type = mode & 0o170000;
            if file_type != 0 && file_type != 0o100000 {
                return Err("Agent skill packs may contain regular files only.".to_string());
            }
        }
        let declared_size = entry.size();
        match entry_name.as_str() {
            "manifest.json" => {
                let content = read_entry_bounded(
                    entry,
                    declared_size,
                    MAX_SKILL_PACK_MANIFEST_BYTES,
                    &mut expanded,
                )?;
                manifest_bytes = Some(content);
            }
            name if name.starts_with("skills/") && name.ends_with("/SKILL.md") => {
                let path_name = name
                    .strip_prefix("skills/")
                    .and_then(|value| value.strip_suffix("/SKILL.md"))
                    .ok_or("Agent skill pack has an invalid skill path.")?;
                validate_name(path_name)?;
                if name != format!("skills/{path_name}/SKILL.md")
                    || skill_files.contains_key(path_name)
                {
                    return Err(
                        "Agent skill pack contains an unsafe or duplicate skill path.".into(),
                    );
                }
                let content =
                    read_entry_bounded(entry, declared_size, MAX_SKILL_BYTES, &mut expanded)?;
                skill_files.insert(path_name.to_string(), content);
            }
            _ => {
                return Err("Agent skill pack contains an unsupported file; only manifest.json and SKILL.md files are accepted.".into());
            }
        }
    }

    let manifest: ParsedSkillPackManifest = serde_json::from_slice(
        &manifest_bytes.ok_or("Agent skill pack is missing manifest.json.")?,
    )
    .map_err(|error| format!("Invalid agent skill pack manifest: {error}"))?;
    if manifest.format != SKILL_PACK_FORMAT || !matches!(manifest.version, 1 | 2) {
        return Err("Unsupported agent skill pack format or version.".to_string());
    }
    if manifest.exported_from.trim().is_empty() || manifest.exported_from.chars().count() > 128 {
        return Err("Agent skill pack provenance must contain 1–128 characters.".to_string());
    }
    validate_visible_text(&manifest.exported_from, "Pack provenance", false)?;
    let names = manifest
        .skills
        .iter()
        .map(|skill| skill.name.clone())
        .collect::<Vec<_>>();
    validate_pack_skill_names(&names)?;
    if manifest.version == 1 && manifest.skills.len() != 1 {
        return Err("Version 1 skill packs must contain exactly one skill.".into());
    }
    if manifest.skills.len() != skill_files.len() {
        return Err("Manifest entries do not match the SKILL.md files in the archive.".into());
    }

    let mut skills = Vec::with_capacity(manifest.skills.len());
    for entry in manifest.skills {
        let expected_file = format!("skills/{}/SKILL.md", entry.name);
        if entry.file != expected_file {
            return Err("Manifest skill path does not match its skill name.".to_string());
        }
        let bytes = skill_files
            .remove(&entry.name)
            .ok_or("Manifest skill is missing its SKILL.md file.")?;
        let content = String::from_utf8(bytes)
            .map_err(|_| "Packed SKILL.md must be valid UTF-8 text.".to_string())?;
        let description = validate_content(&entry.name, &content)?;
        if description != entry.description {
            return Err("Manifest description does not match SKILL.md frontmatter.".to_string());
        }
        let license = skill_license(&content);
        if (manifest.version == 2 || entry.license.is_some()) && entry.license != license {
            return Err("Manifest license does not match SKILL.md frontmatter.".to_string());
        }
        let actual_hash = content_hash(&content);
        if entry.sha256 != actual_hash {
            return Err("Packed SKILL.md checksum does not match its manifest.".to_string());
        }
        skills.push(ParsedSkill {
            name: entry.name,
            description,
            license,
            content,
            content_hash: actual_hash,
        });
    }
    if !skill_files.is_empty() {
        return Err("Archive contains undeclared SKILL.md files.".into());
    }
    if manifest.version == 2 {
        skills.sort_by(|left, right| left.name.cmp(&right.name));
    }
    Ok(ParsedSkillPack {
        version: manifest.version,
        skills,
        exported_from: manifest.exported_from,
    })
}

fn build_skill_pack(skills: &[ParsedSkill]) -> Result<Vec<u8>, String> {
    let mut skills = skills.to_vec();
    skills.sort_by(|left, right| left.name.cmp(&right.name));
    let names = skills
        .iter()
        .map(|skill| skill.name.clone())
        .collect::<Vec<_>>();
    validate_pack_skill_names(&names)?;
    let manifest = SkillPackManifest {
        format: SKILL_PACK_FORMAT,
        version: 2,
        exported_from: "Buzz Skill Library",
        skills: skills
            .iter()
            .map(|skill| SkillPackEntry {
                name: skill.name.clone(),
                description: skill.description.clone(),
                license: skill.license.clone(),
                file: format!("skills/{}/SKILL.md", skill.name),
                sha256: skill.content_hash.clone(),
            })
            .collect(),
    };
    let manifest = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("Encode skill pack manifest: {error}"))?;
    if manifest.len() > MAX_SKILL_PACK_MANIFEST_BYTES {
        return Err("Agent skill pack manifest is too large.".into());
    }
    let expanded_size = manifest.len()
        + skills
            .iter()
            .map(|skill| skill.content.len())
            .sum::<usize>();
    if expanded_size > MAX_SKILL_PACK_EXPANDED_BYTES {
        return Err("Agent skill pack exceeds its expanded-size limit.".into());
    }
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    archive
        .start_file("manifest.json", options)
        .map_err(|error| format!("Start skill pack manifest: {error}"))?;
    archive
        .write_all(&manifest)
        .map_err(|error| format!("Write skill pack manifest: {error}"))?;
    for skill in skills {
        archive
            .start_file(format!("skills/{}/SKILL.md", skill.name), options)
            .map_err(|error| format!("Start skill pack content: {error}"))?;
        archive
            .write_all(skill.content.as_bytes())
            .map_err(|error| format!("Write skill pack content: {error}"))?;
    }
    let bytes = archive
        .finish()
        .map(|cursor| cursor.into_inner())
        .map_err(|error| format!("Finish skill pack: {error}"))?;
    if bytes.len() > MAX_SKILL_PACK_BYTES || expanded_size > MAX_SKILL_PACK_EXPANDED_BYTES {
        return Err("Agent skill pack exceeds its size limit.".into());
    }
    Ok(bytes)
}

fn ensure_text_only_skill_dir(path: &Path) -> Result<(), String> {
    let entries = fs::read_dir(path).map_err(|error| format!("Inspect skill files: {error}"))?;
    let mut found_skill_file = false;
    for entry in entries {
        let entry = entry.map_err(|error| format!("Read skill file entry: {error}"))?;
        if entry.file_name() != "SKILL.md" {
            return Err(
                "Export currently supports text-only skills containing only SKILL.md.".to_string(),
            );
        }
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| format!("Inspect skill file: {error}"))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err("SKILL.md must be a regular file.".to_string());
        }
        found_skill_file = true;
    }
    if !found_skill_file {
        return Err("Skill has no SKILL.md file to export.".to_string());
    }
    Ok(())
}

fn read_skill_file(root: &Path, name: &str) -> Result<(String, String, Option<String>), String> {
    validate_name(name)?;
    let skill_dir = root.join(name);
    let dir_metadata = fs::symlink_metadata(&skill_dir)
        .map_err(|e| format!("Inspect {}: {e}", skill_dir.display()))?;
    if dir_metadata.file_type().is_symlink() || !dir_metadata.is_dir() {
        return Err("Skill folders must be real directories, not symlinks.".to_string());
    }
    let path = root.join(name).join("SKILL.md");
    let metadata =
        fs::symlink_metadata(&path).map_err(|e| format!("Read {}: {e}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("Skill files must be regular files, not symlinks.".to_string());
    }
    if metadata.len() as usize > MAX_SKILL_BYTES {
        return Err(format!(
            "SKILL.md must be smaller than {} KiB.",
            MAX_SKILL_BYTES / 1024
        ));
    }
    let content = fs::read_to_string(&path).map_err(|e| format!("Read {}: {e}", path.display()))?;
    let description = validate_content(name, &content);
    match description {
        Ok(description) => Ok((content, description, None)),
        Err(error) => Ok((content, "Needs review".to_string(), Some(error))),
    }
}

fn runtime_compatibility(root: &Path, name: &str) -> Vec<AgentSkillRuntimeCompatibility> {
    let canonical_skill = root.join(".agents/skills").join(name).canonicalize().ok();
    crate::managed_agents::KNOWN_ACP_RUNTIMES
        .iter()
        .filter_map(|runtime| {
            let skill_directory = runtime.skill_dir?;
            let status = if canonical_skill.is_none() {
                SkillRuntimeLinkStatus::Blocked
            } else {
                match ensure_real_directory(root, Path::new(skill_directory), false) {
                    Err(_) => SkillRuntimeLinkStatus::Blocked,
                    Ok(parent) => match fs::symlink_metadata(parent.join(name)) {
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            SkillRuntimeLinkStatus::Missing
                        }
                        Err(_) => SkillRuntimeLinkStatus::Blocked,
                        Ok(_) => match parent.join(name).canonicalize() {
                            Ok(linked_skill) if canonical_skill.as_ref() == Some(&linked_skill) => {
                                SkillRuntimeLinkStatus::Linked
                            }
                            Ok(_) => SkillRuntimeLinkStatus::Conflict,
                            Err(_) => SkillRuntimeLinkStatus::Blocked,
                        },
                    },
                }
            };
            Some(AgentSkillRuntimeCompatibility {
                runtime_id: runtime.id.to_string(),
                runtime_label: runtime.label.to_string(),
                skill_directory: skill_directory.to_string(),
                status,
            })
        })
        .collect()
}

#[cfg(unix)]
fn install_compatibility_links(root: &Path, name: &str) {
    use crate::util::create_symlink;
    for skill_dir in crate::managed_agents::known_skill_dirs() {
        let relative = Path::new(skill_dir);
        let Ok(parent) = ensure_real_directory(root, relative, true) else {
            continue;
        };
        let link = parent.join(name);
        if fs::symlink_metadata(&link).is_ok() {
            continue;
        }
        let depth = relative.components().count();
        let target = format!("{}{}{}", "../".repeat(depth), ".agents/skills/", name);
        let _ = create_symlink(Path::new(&target), &link);
    }
}

#[cfg(not(unix))]
fn install_compatibility_links(_root: &Path, _name: &str) {}

#[tauri::command]
pub fn list_agent_skills() -> Result<Vec<AgentSkillSummary>, String> {
    let root = skills_root()?;
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("List skills: {error}")),
    };
    let mut skills = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("Read skill entry: {e}"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == MANAGED_SKILL || validate_name(&name).is_err() {
            continue;
        }
        let metadata = entry
            .file_type()
            .map_err(|e| format!("Inspect skill entry: {e}"))?;
        if metadata.is_symlink() || !metadata.is_dir() {
            continue;
        }
        let summary = match read_skill_file(&root, &name) {
            Ok((content, description, validation_error)) => AgentSkillSummary {
                name,
                description,
                content_hash: content_hash(&content),
                validation_error,
            },
            Err(error) => AgentSkillSummary {
                name,
                description: "Needs review".to_string(),
                content_hash: String::new(),
                validation_error: Some(error),
            },
        };
        skills.push(summary);
    }
    skills.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(skills)
}

#[tauri::command]
pub fn read_agent_skill(name: String) -> Result<AgentSkillDetails, String> {
    let skills = skills_root()?;
    let workspace = nest_dir().ok_or("cannot resolve Buzz workspace")?;
    let (content, description, validation_error) = read_skill_file(&skills, &name)?;
    Ok(AgentSkillDetails {
        name: name.clone(),
        content_hash: content_hash(&content),
        runtime_compatibility: runtime_compatibility(&workspace, &name),
        content,
        description,
        validation_error,
    })
}

#[tauri::command]
pub fn save_agent_skill(
    name: String,
    content: String,
    expected_content_hash: Option<String>,
) -> Result<AgentSkillDetails, String> {
    let root = nest_dir().ok_or("cannot resolve Buzz workspace")?;
    save_agent_skill_at(&root, &name, &content, expected_content_hash.as_deref())
}

/// Export reviewed, text-only skills as a deterministic v2 `.agent.zip` pack.
#[tauri::command]
pub async fn export_agent_skill_pack(
    skills: Vec<AgentSkillPackExportRequest>,
    app: AppHandle,
) -> Result<bool, String> {
    let names = skills
        .iter()
        .map(|skill| skill.name.clone())
        .collect::<Vec<_>>();
    validate_pack_skill_names(&names)?;
    let root = skills_root()?;
    let mut pack_skills = Vec::with_capacity(skills.len());
    for reviewed in skills {
        let (content, description, validation_error) = read_skill_file(&root, &reviewed.name)?;
        if let Some(error) = validation_error {
            return Err(format!(
                "Review skill {} before export: {error}",
                reviewed.name
            ));
        }
        if content_hash(&content) != reviewed.expected_content_hash {
            return Err(format!(
                "Skill {} changed on disk since review; inspect it again before exporting.",
                reviewed.name
            ));
        }
        ensure_text_only_skill_dir(&root.join(&reviewed.name))?;
        pack_skills.push(ParsedSkill {
            name: reviewed.name,
            description,
            license: skill_license(&content),
            content_hash: content_hash(&content),
            content,
        });
    }
    let bytes = build_skill_pack(&pack_skills)?;
    let filename = if pack_skills.len() == 1 {
        format!("{}.agent.zip", pack_skills[0].name)
    } else {
        "skills.agent.zip".to_string()
    };
    save_bytes_with_dialog(&app, &filename, "Agent skill pack", &["zip"], &bytes).await
}

/// Validate a `.agent.zip` pack and return its full contents for explicit review.
#[tauri::command]
pub fn preview_agent_skill_pack(file_bytes: Vec<u8>) -> Result<AgentSkillPackPreview, String> {
    let pack = parse_skill_pack(&file_bytes)?;
    let root = skills_root()?;
    let installed = installed_skill_name_keys(&root)?;
    let skills = pack
        .skills
        .into_iter()
        .map(|skill| AgentSkillPackSkillPreview {
            already_installed: installed.contains(&skill_name_key(&skill.name)),
            name: skill.name,
            description: skill.description,
            license: skill.license,
            content: skill.content,
            content_hash: skill.content_hash,
        })
        .collect();
    Ok(AgentSkillPackPreview {
        version: pack.version,
        exported_from: pack.exported_from,
        skills,
    })
}

/// Install only the exact reviewed pack, refusing to overwrite an existing skill.
#[tauri::command]
pub fn install_agent_skill_pack(
    file_bytes: Vec<u8>,
    expected_skills: Vec<ReviewedAgentSkill>,
) -> Result<Vec<AgentSkillDetails>, String> {
    let pack = parse_skill_pack(&file_bytes)?;
    if !review_matches(&pack.skills, &expected_skills) {
        return Err(
            "This pack changed after preview. Inspect it again before installing.".to_string(),
        );
    }
    let root = nest_dir().ok_or("cannot resolve Buzz workspace")?;
    install_skill_pack_at(&root, &pack.skills)
}

fn review_matches(skills: &[ParsedSkill], expected: &[ReviewedAgentSkill]) -> bool {
    if skills.len() != expected.len() || expected.is_empty() || expected.len() > MAX_SKILLS_PER_PACK
    {
        return false;
    }
    let mut reviewed = BTreeMap::new();
    for item in expected {
        if reviewed
            .insert(
                skill_name_key(&item.name),
                (item.name.clone(), item.content_hash.clone()),
            )
            .is_some()
        {
            return false;
        }
    }
    skills.iter().all(|skill| {
        reviewed
            .get(&skill_name_key(&skill.name))
            .is_some_and(|(name, hash)| name == &skill.name && hash == &skill.content_hash)
    })
}

fn installed_skill_name_keys(root: &Path) -> Result<BTreeSet<String>, String> {
    let path = ensure_real_directory(root, Path::new(".agents/skills"), false)?;
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => return Err(format!("List installed skills: {error}")),
    };
    let mut names = BTreeSet::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("Read installed skill entry: {error}"))?;
        names.insert(skill_name_key(&entry.file_name().to_string_lossy()));
    }
    Ok(names)
}

fn ensure_skills_directory_with_tracking(root: &Path) -> Result<(PathBuf, Vec<PathBuf>), String> {
    let root_metadata =
        fs::symlink_metadata(root).map_err(|error| format!("Inspect Buzz workspace: {error}"))?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err("Buzz workspace must be a real directory.".into());
    }
    let mut current = root.to_path_buf();
    let mut created = Vec::new();
    for component in [".agents", "skills"] {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let rollback = remove_empty_created_directories(&created);
                return Err(rollback_error(
                    format!("Refusing to follow a symlink at {}", current.display()),
                    &rollback,
                ));
            }
            Ok(metadata) if !metadata.is_dir() => {
                let rollback = remove_empty_created_directories(&created);
                return Err(rollback_error(
                    format!("Expected a directory at {}", current.display()),
                    &rollback,
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if let Err(create_error) = fs::create_dir(&current) {
                    let rollback = remove_empty_created_directories(&created);
                    return Err(rollback_error(
                        format!("Create {}: {create_error}", current.display()),
                        &rollback,
                    ));
                }
                created.push(current.clone());
            }
            Err(error) => {
                let rollback = remove_empty_created_directories(&created);
                return Err(rollback_error(
                    format!("Inspect {}: {error}", current.display()),
                    &rollback,
                ));
            }
        }
    }
    Ok((current, created))
}

fn remove_empty_created_directories(paths: &[PathBuf]) -> Vec<String> {
    let mut failures = Vec::new();
    for path in paths.iter().rev() {
        match fs::remove_dir(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => failures.push(format!("{} ({error})", path.display())),
        }
    }
    failures
}

fn rollback_error(primary: String, failures: &[String]) -> String {
    if failures.is_empty() {
        primary
    } else {
        format!(
            "{primary}; rollback incomplete for: {}",
            failures.join(", ")
        )
    }
}

fn rollback_created_skill_dirs(skills_root: &Path, created: &[(String, String)]) -> Vec<String> {
    let mut failures = Vec::new();
    for (name, expected_hash) in created.iter().rev() {
        let dir = skills_root.join(name);
        let metadata = match fs::symlink_metadata(&dir) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                failures.push(format!(
                    "{} is no longer the created directory",
                    dir.display()
                ));
                continue;
            }
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                failures.push(format!("{} ({error})", dir.display()));
                continue;
            }
        };
        let _ = metadata;
        let mut entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) => {
                failures.push(format!("{} ({error})", dir.display()));
                continue;
            }
        };
        let first = match entries.next() {
            Some(Ok(entry)) if entry.file_name() == "SKILL.md" => entry,
            None => {
                if let Err(error) = fs::remove_dir(&dir) {
                    failures.push(format!("{} ({error})", dir.display()));
                }
                continue;
            }
            Some(Ok(_)) => {
                failures.push(format!("{} contains unrelated content", dir.display()));
                continue;
            }
            Some(Err(error)) => {
                failures.push(format!("{} ({error})", dir.display()));
                continue;
            }
        };
        if entries.next().is_some() {
            failures.push(format!("{} contains unrelated content", dir.display()));
            continue;
        }
        let file = first.path();
        let file_metadata = match fs::symlink_metadata(&file) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                failures.push(format!(
                    "{} is not the created regular file",
                    file.display()
                ));
                continue;
            }
            Ok(metadata) => metadata,
            Err(error) => {
                failures.push(format!("{} ({error})", file.display()));
                continue;
            }
        };
        let _ = file_metadata;
        let content = match fs::read(&file) {
            Ok(content) => content,
            Err(error) => {
                failures.push(format!("{} ({error})", file.display()));
                continue;
            }
        };
        if hex::encode(Sha256::digest(&content)) != *expected_hash {
            failures.push(format!(
                "{} no longer matches the installed pack",
                file.display()
            ));
            continue;
        }
        if let Err(error) = fs::remove_file(&file) {
            failures.push(format!("{} ({error})", file.display()));
            continue;
        }
        if let Err(error) = fs::remove_dir(&dir) {
            failures.push(format!("{} ({error})", dir.display()));
        }
    }
    failures
}

fn install_skill_pack_at(
    root: &Path,
    skills: &[ParsedSkill],
) -> Result<Vec<AgentSkillDetails>, String> {
    install_skill_pack_at_with_failure(root, skills, None)
}

fn install_skill_pack_at_with_failure(
    root: &Path,
    skills: &[ParsedSkill],
    fail_after_commits: Option<usize>,
) -> Result<Vec<AgentSkillDetails>, String> {
    let names = skills
        .iter()
        .map(|skill| skill.name.clone())
        .collect::<Vec<_>>();
    validate_pack_skill_names(&names)?;
    for skill in skills {
        if validate_content(&skill.name, &skill.content)? != skill.description
            || content_hash(&skill.content) != skill.content_hash
        {
            return Err("A skill changed after pack validation.".into());
        }
    }

    let _write_lock = lock_skill_writes(root)?;
    let (skills_root, created_parents) = ensure_skills_directory_with_tracking(root)?;
    let mut created = Vec::<(String, String)>::new();
    let result = (|| -> Result<(), String> {
        let installed = installed_skill_name_keys(root)?;
        if names
            .iter()
            .any(|name| installed.contains(&skill_name_key(name)))
        {
            return Err(
                "A skill with the same name already exists; no skills were installed.".into(),
            );
        }

        let staging = tempfile::tempdir_in(&skills_root)
            .map_err(|error| format!("Prepare skill pack install: {error}"))?;
        let mut staged_files = Vec::with_capacity(skills.len());
        for skill in skills {
            let path = staging.path().join(format!("{}.md", skill.name));
            let mut file = fs::File::create(&path)
                .map_err(|error| format!("Stage skill {}: {error}", skill.name))?;
            file.write_all(skill.content.as_bytes())
                .map_err(|error| format!("Stage skill {}: {error}", skill.name))?;
            file.sync_all()
                .map_err(|error| format!("Sync staged skill {}: {error}", skill.name))?;
            staged_files.push(path);
        }

        for (index, (skill, staged_file)) in skills.iter().zip(&staged_files).enumerate() {
            let collision = installed_skill_name_keys(root)?.contains(&skill_name_key(&skill.name));
            if collision {
                return Err(format!(
                    "A skill named {} appeared during install.",
                    skill.name
                ));
            }
            let dir = skills_root.join(&skill.name);
            match fs::create_dir(&dir) {
                Ok(()) => created.push((skill.name.clone(), skill.content_hash.clone())),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    return Err(format!("A skill named {} already exists.", skill.name));
                }
                Err(error) => return Err(format!("Create skill {}: {error}", skill.name)),
            }
            let install_result = (|| {
                let mut temporary = tempfile::NamedTempFile::new_in(&dir)
                    .map_err(|error| format!("Prepare skill {}: {error}", skill.name))?;
                let mut source = fs::File::open(staged_file)
                    .map_err(|error| format!("Read staged skill {}: {error}", skill.name))?;
                std::io::copy(&mut source, &mut temporary)
                    .map_err(|error| format!("Write skill {}: {error}", skill.name))?;
                temporary
                    .as_file()
                    .sync_all()
                    .map_err(|error| format!("Sync skill {}: {error}", skill.name))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    temporary
                        .as_file()
                        .set_permissions(fs::Permissions::from_mode(0o600))
                        .map_err(|error| format!("Protect skill {}: {error}", skill.name))?;
                }
                temporary
                    .persist_noclobber(dir.join("SKILL.md"))
                    .map_err(|error| {
                        format!(
                            "Install skill {} without overwrite: {}",
                            skill.name, error.error
                        )
                    })?;
                Ok::<(), String>(())
            })();
            if let Err(error) = install_result {
                return Err(error);
            }
            if fail_after_commits == Some(index + 1) {
                return Err("Injected failure after committed skill.".into());
            }
        }
        Ok(())
    })();

    if let Err(error) = result {
        let mut rollback = rollback_created_skill_dirs(&skills_root, &created);
        rollback.extend(remove_empty_created_directories(&created_parents));
        return Err(rollback_error(error, &rollback));
    }

    for skill in skills {
        install_compatibility_links(root, &skill.name);
    }
    let details = skills
        .iter()
        .map(|skill| AgentSkillDetails {
            name: skill.name.clone(),
            description: skill.description.clone(),
            content: skill.content.clone(),
            content_hash: skill.content_hash.clone(),
            validation_error: None,
            runtime_compatibility: runtime_compatibility(root, &skill.name),
        })
        .collect();
    Ok(details)
}

fn save_agent_skill_at(
    root: &Path,
    name: &str,
    content: &str,
    expected_content_hash: Option<&str>,
) -> Result<AgentSkillDetails, String> {
    validate_name(&name)?;
    let description = validate_content(&name, &content)?;
    let _write_lock = lock_skill_writes(root)?;
    let skills = ensure_real_directory(root, Path::new(".agents/skills"), true)?;
    let dir = skills.join(&name);

    if let Some(expected_hash) = expected_content_hash {
        let (current, _, _) = read_skill_file(&skills, &name)?;
        if content_hash(&current) != expected_hash {
            return Err(
                "This skill changed on disk since you opened it. Reload it before saving."
                    .to_string(),
            );
        }
    } else {
        match fs::create_dir(&dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err("A skill with that name already exists. Select it to edit.".to_string());
            }
            Err(error) => return Err(format!("Create skill directory: {error}")),
        }
    }

    let file = dir.join("SKILL.md");
    if file
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err("Refusing to overwrite a symlinked SKILL.md.".to_string());
    }
    let mut temporary =
        tempfile::NamedTempFile::new_in(&dir).map_err(|e| format!("Prepare skill write: {e}"))?;
    temporary
        .write_all(content.as_bytes())
        .map_err(|e| format!("Write skill: {e}"))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|e| format!("Sync skill: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("Protect skill file: {e}"))?;
    }
    temporary
        .persist(&file)
        .map_err(|e| format!("Save {}: {}", file.display(), e.error))?;
    install_compatibility_links(root, &name);

    Ok(AgentSkillDetails {
        name: name.to_string(),
        content_hash: content_hash(&content),
        runtime_compatibility: runtime_compatibility(root, &name),
        content: content.to_string(),
        description,
        validation_error: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const SKILL: &str = "---\nname: plan-review\ndescription: Review project plans for missing assumptions.\n---\n\n# Plan review\n\nCheck evidence and risks.\n";

    #[test]
    fn validates_agent_skills_frontmatter_and_identity() {
        assert_eq!(
            validate_content("plan-review", SKILL).unwrap(),
            "Review project plans for missing assumptions."
        );
        assert!(validate_content("other", SKILL)
            .unwrap_err()
            .contains("must match"));
        assert!(validate_content("plan-review", "# no frontmatter").is_err());
    }

    #[test]
    fn accepts_unicode_names_and_valid_optional_frontmatter() {
        let content = "---\nname: réview\ndescription: Review a project plan.\nlicense: Apache-2.0\ncompatibility: Requires a local git checkout.\nmetadata:\n  author: example-org\n  version: \"1.0\"\nallowed-tools: Read Bash(git:*)\n---\n\n# Review\n";
        assert_eq!(
            validate_content("réview", content).unwrap(),
            "Review a project plan."
        );
        assert!(validate_name(&"é".repeat(64)).is_ok());
        assert!(validate_name(&"é".repeat(65)).is_err());

        let temp = tempdir().unwrap();
        let saved = save_agent_skill_at(temp.path(), "réview", content, None).unwrap();
        assert_eq!(saved.name, "réview");
        let skills =
            ensure_real_directory(temp.path(), Path::new(".agents/skills"), false).unwrap();
        let disk_name = fs::read_dir(skills)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .file_name()
            .into_string()
            .unwrap();
        assert!(validate_content(&disk_name, content).is_ok());
    }

    #[test]
    fn rejects_malformed_optional_frontmatter() {
        let too_long_compatibility = SKILL.replace(
            "---\n\n#",
            &format!("compatibility: {}\n---\n\n#", "x".repeat(501)),
        );
        let invalid_optional_fields = [
            SKILL.replace("---\n\n#", "license: []\n---\n\n#"),
            SKILL.replace("---\n\n#", "allowed-tools: [Read]\n---\n\n#"),
            SKILL.replace("---\n\n#", "metadata:\n  version: 1\n---\n\n#"),
            SKILL.replace("---\n\n#", "metadata: []\n---\n\n#"),
            too_long_compatibility,
        ];

        for content in invalid_optional_fields {
            assert!(validate_content("plan-review", &content).is_err());
        }
    }

    #[test]
    fn rejects_hidden_text_and_path_shaped_names() {
        assert!(validate_content(
            "plan-review",
            &SKILL.replace("evidence", "evid\u{202e}ence")
        )
        .is_err());
        assert!(validate_name("../outside").is_err());
        assert!(validate_name("buzz-cli").is_err());
    }

    #[test]
    fn saves_new_skill_and_requires_matching_revision_for_updates() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        let saved = save_agent_skill_at(root, "plan-review", SKILL, None).unwrap();
        assert_eq!(saved.name, "plan-review");
        assert_eq!(
            saved.description,
            "Review project plans for missing assumptions."
        );

        let revised = SKILL.replace(
            "Check evidence and risks.",
            "Check evidence, assumptions, and risks.",
        );
        assert!(
            save_agent_skill_at(root, "plan-review", &revised, Some("stale-hash"))
                .unwrap_err()
                .contains("changed on disk")
        );
        let updated =
            save_agent_skill_at(root, "plan-review", &revised, Some(&saved.content_hash)).unwrap();
        assert_eq!(updated.content, revised);
    }

    #[test]
    fn concurrent_updates_cannot_overwrite_the_same_reviewed_revision() {
        use std::sync::{Arc, Barrier};

        let temp = tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let saved = save_agent_skill_at(&root, "plan-review", SKILL, None).unwrap();
        let expected_hash = saved.content_hash;
        let first_content = SKILL.replace("Check evidence and risks.", "First concurrent edit.");
        let second_content = SKILL.replace("Check evidence and risks.", "Second concurrent edit.");
        let barrier = Arc::new(Barrier::new(3));

        let first = {
            let barrier = Arc::clone(&barrier);
            let root = root.clone();
            let expected_hash = expected_hash.clone();
            std::thread::spawn(move || {
                barrier.wait();
                save_agent_skill_at(&root, "plan-review", &first_content, Some(&expected_hash))
            })
        };
        let second = {
            let barrier = Arc::clone(&barrier);
            let root = root.clone();
            std::thread::spawn(move || {
                barrier.wait();
                save_agent_skill_at(&root, "plan-review", &second_content, Some(&expected_hash))
            })
        };
        barrier.wait();
        let results = [first.join().unwrap(), second.join().unwrap()];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
        let (current, _, _) = read_skill_file(
            &ensure_real_directory(&root, Path::new(".agents/skills"), false).unwrap(),
            "plan-review",
        )
        .unwrap();
        assert!(current.contains("concurrent edit."));
    }

    #[cfg(unix)]
    #[test]
    fn runtime_compatibility_reports_only_catalog_paths_and_live_link_state() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let skills = ensure_real_directory(temp.path(), Path::new(".agents/skills"), true).unwrap();
        let skill_dir = skills.join("plan-review");
        fs::create_dir(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), SKILL).unwrap();

        let missing = runtime_compatibility(temp.path(), "plan-review");
        assert_eq!(missing.len(), 3);
        assert!(missing
            .iter()
            .all(|runtime| matches!(runtime.status, SkillRuntimeLinkStatus::Missing)));

        install_compatibility_links(temp.path(), "plan-review");
        let linked = runtime_compatibility(temp.path(), "plan-review");
        assert!(
            linked
                .iter()
                .all(|runtime| matches!(runtime.status, SkillRuntimeLinkStatus::Linked)),
            "unexpected runtime links: {linked:#?}"
        );
        assert!(linked.iter().any(|runtime| {
            runtime.runtime_id == "codex" && runtime.skill_directory == ".codex/skills"
        }));

        let codex_link = temp.path().join(".codex/skills/plan-review");
        fs::remove_file(&codex_link).unwrap();
        let foreign_skill = temp.path().join("foreign-skill");
        fs::create_dir(&foreign_skill).unwrap();
        symlink(&foreign_skill, &codex_link).unwrap();
        let conflicted = runtime_compatibility(temp.path(), "plan-review");
        assert!(conflicted.iter().any(|runtime| {
            runtime.runtime_id == "codex"
                && matches!(runtime.status, SkillRuntimeLinkStatus::Conflict)
        }));
    }

    #[cfg(unix)]
    #[test]
    fn listing_ignores_symlinked_skill_directories() {
        use std::os::unix::fs::symlink;
        let temp = tempdir().unwrap();
        let root = temp.path().join("skills");
        fs::create_dir(&root).unwrap();
        let target = temp.path().join("outside");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("SKILL.md"), SKILL).unwrap();
        symlink(&target, root.join("plan-review")).unwrap();
        assert!(read_skill_file(&root, "plan-review")
            .unwrap_err()
            .contains("real directories"));
    }

    fn parsed_skill(name: &str, content: &str) -> ParsedSkill {
        ParsedSkill {
            name: name.to_string(),
            description: validate_content(name, content).unwrap(),
            license: skill_license(content),
            content: content.to_string(),
            content_hash: content_hash(content),
        }
    }

    fn raw_pack(manifest: serde_json::Value, files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        archive.start_file("manifest.json", options).unwrap();
        archive
            .write_all(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        for (name, bytes) in files {
            archive.start_file(*name, options).unwrap();
            archive.write_all(bytes).unwrap();
        }
        archive.finish().unwrap().into_inner()
    }

    fn manifest_entry(name: &str, content: &str, license: Option<&str>) -> serde_json::Value {
        let mut value = serde_json::json!({
            "name": name,
            "description": validate_content(name, content).unwrap(),
            "file": format!("skills/{name}/SKILL.md"),
            "sha256": content_hash(content),
        });
        if let Some(license) = license {
            value["license"] = license.into();
        }
        value
    }

    fn v1_pack(name: &str, content: &str) -> Vec<u8> {
        raw_pack(
            serde_json::json!({
                "format": SKILL_PACK_FORMAT,
                "version": 1,
                "exportedFrom": "Buzz Skill Library",
                "skills": [manifest_entry(name, content, None)],
            }),
            &[("skills/plan-review/SKILL.md", content.as_bytes())],
        )
    }

    #[test]
    fn imports_v1_single_skill_packs_and_normalizes_preview_shape() {
        let licensed = SKILL.replace("---\n\n#", "license: Apache-2.0\n---\n\n#");
        let bytes = v1_pack("plan-review", &licensed);
        let pack = parse_skill_pack(&bytes).unwrap();
        assert_eq!(pack.version, 1);
        assert_eq!(pack.skills.len(), 1);
        assert_eq!(pack.skills[0].license.as_deref(), Some("Apache-2.0"));
        assert_eq!(pack.skills[0].content, licensed);
    }

    #[test]
    fn v2_round_trip_sorts_skills_and_checks_license() {
        let first = parsed_skill("zeta", &SKILL.replace("plan-review", "zeta"));
        let second = parsed_skill("alpha", &SKILL.replace("plan-review", "alpha"));
        let bytes = build_skill_pack(&[first, second]).unwrap();
        let pack = parse_skill_pack(&bytes).unwrap();
        assert_eq!(pack.version, 2);
        assert_eq!(pack.exported_from, "Buzz Skill Library");
        assert_eq!(
            pack.skills
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "zeta"]
        );
        let licensed = SKILL.replace("---\n\n#", "license: Apache-2.0\n---\n\n#");
        let mut manifest = serde_json::json!({"format":SKILL_PACK_FORMAT,"version":2,"exportedFrom":"Buzz Skill Library","skills":[manifest_entry("plan-review", &licensed, Some("MIT"))]});
        let bytes = raw_pack(
            manifest.clone(),
            &[("skills/plan-review/SKILL.md", licensed.as_bytes())],
        );
        assert!(parse_skill_pack(&bytes).unwrap_err().contains("license"));
        manifest["skills"][0]["license"] = "Apache-2.0".into();
        assert!(parse_skill_pack(&raw_pack(
            manifest,
            &[("skills/plan-review/SKILL.md", licensed.as_bytes())]
        ))
        .is_ok());
    }

    #[test]
    fn rejects_duplicate_names_case_collisions_unsafe_and_extra_paths() {
        let entry = manifest_entry("plan-review", SKILL, None);
        let duplicate = serde_json::json!({"format":SKILL_PACK_FORMAT,"version":2,"exportedFrom":"Buzz Skill Library","skills":[entry.clone(),entry.clone()]});
        let files = [("skills/plan-review/SKILL.md", SKILL.as_bytes())];
        assert!(parse_skill_pack(&raw_pack(duplicate, &files))
            .unwrap_err()
            .contains("duplicate"));
        let unsafe_pack = raw_pack(
            serde_json::json!({"format":SKILL_PACK_FORMAT,"version":2,"exportedFrom":"Buzz Skill Library","skills":[entry]}),
            &[("skills/../plan-review/SKILL.md", SKILL.as_bytes())],
        );
        assert!(parse_skill_pack(&unsafe_pack).is_err());
        let extra = raw_pack(
            serde_json::json!({"format":SKILL_PACK_FORMAT,"version":2,"exportedFrom":"Buzz Skill Library","skills":[manifest_entry("plan-review", SKILL, None)]}),
            &[
                ("skills/plan-review/SKILL.md", SKILL.as_bytes()),
                ("extra.txt", b"x"),
            ],
        );
        assert!(parse_skill_pack(&extra)
            .unwrap_err()
            .contains("unsupported file"));
        assert!(
            validate_pack_skill_names(&["plan-review".to_string(), "Plan-Review".to_string()])
                .unwrap_err()
                .contains("Skill names must")
        );
    }

    #[test]
    fn enforces_archive_and_aggregate_expanded_limits_and_checks_hashes() {
        assert!(parse_skill_pack(&vec![0; MAX_SKILL_PACK_BYTES + 1])
            .unwrap_err()
            .contains("1 MiB"));
        let mut entries = Vec::new();
        let mut files = Vec::new();
        let contents: Vec<String> = (0..5)
            .map(|i| {
                format!(
                    "---\nname: skill-{i}\ndescription: valid skill description\n---\n\n{}",
                    "x".repeat(230_000)
                )
            })
            .collect();
        for (i, content) in contents.iter().enumerate() {
            let name = format!("skill-{i}");
            entries.push(manifest_entry(&name, content, None));
            files.push((format!("skills/{name}/SKILL.md"), content.as_bytes()));
        }
        let manifest = serde_json::json!({"format":SKILL_PACK_FORMAT,"version":2,"exportedFrom":"Buzz Skill Library","skills":entries});
        let borrowed = files
            .iter()
            .map(|(n, b)| (n.as_str(), *b))
            .collect::<Vec<_>>();
        assert!(parse_skill_pack(&raw_pack(manifest, &borrowed))
            .unwrap_err()
            .contains("expanded-size"));
        let mut bad = serde_json::json!({"format":SKILL_PACK_FORMAT,"version":2,"exportedFrom":"Buzz Skill Library","skills":[manifest_entry("plan-review", SKILL, None)]});
        bad["skills"][0]["sha256"] = "0".repeat(64).into();
        assert!(parse_skill_pack(&raw_pack(
            bad,
            &[("skills/plan-review/SKILL.md", SKILL.as_bytes())]
        ))
        .unwrap_err()
        .contains("checksum"));
    }

    #[test]
    fn exact_review_set_and_collision_preflight_protect_install() {
        let skills = vec![
            parsed_skill("plan-review", SKILL),
            parsed_skill(
                "release-check",
                &SKILL.replace("plan-review", "release-check"),
            ),
        ];
        assert!(
            review_matches(
                &skills,
                &[ReviewedAgentSkill {
                    name: "plan-review".into(),
                    content_hash: content_hash(SKILL)
                }]
            ) == false
        );
        let temp = tempdir().unwrap();
        let existing =
            ensure_real_directory(temp.path(), Path::new(".agents/skills/Plan-Review"), true)
                .unwrap();
        fs::write(existing.join("SKILL.md"), SKILL).unwrap();
        assert!(install_skill_pack_at(temp.path(), &skills)
            .unwrap_err()
            .contains("already exists"));
        assert!(!temp.path().join(".agents/skills/release-check").exists());
    }

    #[test]
    fn partial_pack_install_rolls_back_only_its_created_files() {
        let temp = tempdir().unwrap();
        let skills = vec![
            parsed_skill("plan-review", SKILL),
            parsed_skill(
                "release-check",
                &SKILL.replace("plan-review", "release-check"),
            ),
        ];
        let error = install_skill_pack_at_with_failure(temp.path(), &skills, Some(1)).unwrap_err();
        assert!(error.contains("Injected failure"));
        assert!(!temp.path().join(".agents/skills/plan-review").exists());
        assert!(!temp.path().join(".agents/skills/release-check").exists());
        let skill_root = temp.path().join(".agents/skills");
        assert!(!skill_root.exists());
    }

    #[test]
    fn rollback_preserves_files_that_no_longer_match_created_content() {
        let temp = tempdir().unwrap();
        let skills_root =
            ensure_real_directory(temp.path(), Path::new(".agents/skills"), true).unwrap();
        let dir = skills_root.join("plan-review");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), "changed after commit").unwrap();
        let failures = rollback_created_skill_dirs(
            &skills_root,
            &[("plan-review".into(), content_hash(SKILL))],
        );
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("no longer matches"));
        assert_eq!(
            fs::read_to_string(dir.join("SKILL.md")).unwrap(),
            "changed after commit"
        );
    }

    #[test]
    fn skill_pack_export_refuses_unreviewed_extra_files() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("SKILL.md"), SKILL).unwrap();
        fs::write(temp.path().join("notes.txt"), "not in the reviewed export").unwrap();
        assert!(ensure_text_only_skill_dir(temp.path())
            .unwrap_err()
            .contains("only SKILL.md"));
    }
}
