//! Strict, data-only validation for immutable `content-pack/v1` bundles.
//!
//! Validation is deliberately read-only. It never evaluates templates, executes pack
//! code, resolves a URL, or follows a symlink. A caller may activate a pack only when
//! [`ValidationReport::activation_allowed`] returns `true`; every other result is a
//! quarantine report that is safe to display or persist.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    path::{Component, Path, PathBuf},
};

use manchester_dnd_core::hero::{
    CORE_CONTENT_PACK_DIGEST, CORE_CONTENT_PACK_ID, EMBERLINE_THEME_PACK_DIGEST,
    EMBERLINE_THEME_PACK_ID, MVP_PACK_VERSION, RAINBOUND_THEME_PACK_DIGEST,
    RAINBOUND_THEME_PACK_ID,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

pub const CONTENT_PACK_SCHEMA: &str = "content-pack/v1";
pub const CONTENT_DOCUMENT_SCHEMA: &str = "content-document/v1";
pub const TRACEABILITY_SCHEMA: &str = "mechanics-traceability/v1";
pub const PROVENANCE_SCHEMA: &str = "content-provenance/v1";
pub const THEME_TOKENS_SCHEMA: &str = "theme-tokens/v1";
pub const PACK_MANIFEST_FILE: &str = "manifest.json";
pub const MVP_ENGINE_RULESET_ID: &str = "srd-5.1-cc";

/// Engine-authored capability allowlist. Never derive this from pack manifests:
/// doing so would make capability validation tautological. Only the named fixed-
/// encounter class actions and two live spells are exposed; rests, equipment
/// use, and the other four allowlisted spells remain deliberately absent.
pub const MVP_ENGINE_CAPABILITIES: &[&str] = &[
    "advancement.class.fighter.level-2",
    "advancement.class.wizard.level-2",
    "check.basic",
    "combat.attack",
    "combat.damage",
    "combat.initiative",
    "combat.movement",
    "combat.turns",
    "creator.ability.standard-array",
    "creator.ancestry.human",
    "creator.background.sage",
    "creator.background.soldier",
    "creator.class.fighter",
    "creator.class.wizard",
    "creator.equipment.mvp-q04",
    "creator.spell-selection.wizard-q04",
    "encounter.class-actions.fighter-q04",
    "encounter.context-action",
    "encounter.fixed-q04",
    "encounter.objectives",
    "encounter.spells.live-q04",
    "encounter.transition",
    "health.death-saves",
    "health.hit-points",
    "health.story-recovery",
    "health.temporary-hit-points",
    "rng.chacha20-v1",
];

const ALLOWLISTED_CONTENT_ROOTS: &[&str] = &[
    "adventures",
    "definitions",
    "fixtures",
    "mechanics",
    "notices",
    "themes",
];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackVersion(String);

impl PackVersion {
    pub fn parse(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        if !is_valid_semver(&value) {
            return Err("version must be canonical semantic version syntax");
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PackVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for PackVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PackVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sha256Digest(String);

impl Sha256Digest {
    pub fn parse(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        let Some(hex) = value.strip_prefix("sha256:") else {
            return Err("digest must use the sha256: prefix");
        };
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("digest must contain 64 lowercase hexadecimal characters");
        }
        Ok(Self(value))
    }

    pub fn of_bytes(bytes: &[u8]) -> Self {
        let digest = Sha256::digest(bytes);
        let mut value = String::with_capacity(71);
        value.push_str("sha256:");
        for byte in digest {
            use fmt::Write as _;
            write!(&mut value, "{byte:02x}").expect("writing to String cannot fail");
        }
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackCategory {
    Adventure,
    CharacterContent,
    CreatureContent,
    RulesCompendium,
    Theme,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestFileKind {
    Content,
    MechanicsTraceability,
    Notice,
    ThemeTokens,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackDependency {
    pub id: String,
    pub version: PackVersion,
    pub digest: Sha256Digest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackLicense {
    pub license_id: String,
    pub license_url: Option<String>,
    pub notice_path: String,
    pub allowed_content_license_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManifestFile {
    pub path: String,
    pub digest: Sha256Digest,
    pub kind: ManifestFileKind,
    pub provenance_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackManifest {
    pub pack_schema: String,
    pub id: String,
    pub version: PackVersion,
    pub digest: Sha256Digest,
    pub display_name: String,
    pub categories: Vec<PackCategory>,
    pub compatible_rulesets: Vec<String>,
    pub required_engine_capabilities: Vec<String>,
    pub dependencies: Vec<PackDependency>,
    pub license: PackLicense,
    pub provenance_manifest: String,
    pub provenance_digest: Sha256Digest,
    pub content_roots: Vec<String>,
    pub files: Vec<ManifestFile>,
}

/// Hashes the canonical JSON form of every manifest field except `digest` itself.
/// Payload and provenance hashes are fields in that input, so one pin binds the
/// complete pack without requiring an impossible self-referential file hash.
pub fn compute_manifest_digest(manifest: &PackManifest) -> Sha256Digest {
    let mut value = serde_json::to_value(manifest)
        .expect("serializing a PackManifest into a JSON value cannot fail");
    value
        .as_object_mut()
        .expect("PackManifest serializes as an object")
        .remove("digest");
    let bytes =
        serde_json::to_vec(&value).expect("serializing a PackManifest JSON value cannot fail");
    Sha256Digest::of_bytes(&bytes)
}

/// Exercises every strict in-memory content-pack JSON schema. Filesystem
/// traversal and digest/inventory behavior are covered by repository tests;
/// this boundary intentionally performs no I/O.
#[cfg(feature = "fuzzing")]
pub fn fuzz_pack_json(bytes: &[u8]) {
    if bytes.len() > ValidationLimits::default().max_file_bytes as usize {
        return;
    }
    if let Ok(manifest) = serde_json::from_slice::<PackManifest>(bytes) {
        let _ = compute_manifest_digest(&manifest);
    }
    let _ = serde_json::from_slice::<ContentDocument>(bytes);
    let _ = serde_json::from_slice::<MechanicsTraceability>(bytes);
    let _ = serde_json::from_slice::<ProvenanceManifest>(bytes);
    let _ = serde_json::from_slice::<ThemeTokens>(bytes);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentAvailability {
    Active,
    Planned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    Action,
    Ancestry,
    Attack,
    Background,
    Character,
    Class,
    Creature,
    Encounter,
    Equipment,
    Feature,
    Objective,
    Rule,
    Scene,
    Spell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LicenseClass {
    OriginalPrivateEvaluation,
    Srd5_1Cc,
}

impl LicenseClass {
    fn expected_license_id(self) -> &'static str {
        match self {
            Self::OriginalPrivateEvaluation => "LicenseRef-Manchester-Arcana-Private-Evaluation",
            Self::Srd5_1Cc => "CC-BY-4.0",
        }
    }
}

/// Closed metadata about which authoritative Rust policy an entry composes.
/// These values are never interpreted as scripts or arithmetic expressions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TypedEffect {
    EnginePolicy { capability: String },
    Composes { content_ids: Vec<String> },
    Offers { content_ids: Vec<String> },
    TransitionsTo { content_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContentEntry {
    pub schema_version: u16,
    pub id: String,
    pub kind: ContentKind,
    pub availability: ContentAvailability,
    pub display_name: String,
    pub description: String,
    pub ruleset_id: String,
    pub required_engine_capabilities: Vec<String>,
    pub references: Vec<String>,
    pub source_key: String,
    pub license_class: LicenseClass,
    pub provenance_key: String,
    pub effects: Vec<TypedEffect>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContentDocument {
    pub content_schema: String,
    pub document_id: String,
    pub entries: Vec<ContentEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TraceabilityEntry {
    pub mechanic_id: String,
    pub availability: ContentAvailability,
    pub source_key: String,
    pub source_location: String,
    pub license_class: LicenseClass,
    pub provenance_key: String,
    pub implementation_symbols: Vec<String>,
    pub test_ids: Vec<String>,
    pub consuming_content: Vec<String>,
    pub required_engine_capabilities: Vec<String>,
    pub modification_note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MechanicsTraceability {
    pub traceability_schema: String,
    pub pack_id: String,
    pub pack_version: PackVersion,
    pub entries: Vec<TraceabilityEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceOrigin {
    Mixed,
    Original,
    Srd5_1Cc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Approved,
    Draft,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProvenanceEntry {
    pub provenance_key: String,
    pub path: Option<String>,
    pub digest: Option<Sha256Digest>,
    pub subject_ids: Vec<String>,
    pub origin: ProvenanceOrigin,
    pub title: String,
    pub creator: String,
    pub rightsholder: String,
    pub source_locator: String,
    pub license_id: String,
    pub license_url: Option<String>,
    pub required_notice: String,
    pub modification_note: String,
    pub created_or_retrieved_at: String,
    pub reviewer: String,
    pub review_status: ReviewStatus,
    pub ruleset_ids: Vec<String>,
    pub pack_id: String,
    pub pack_version: PackVersion,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProvenanceManifest {
    pub provenance_schema: String,
    pub pack_id: String,
    pub pack_version: PackVersion,
    pub entries: Vec<ProvenanceEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ThemePalette {
    pub background: String,
    pub surface: String,
    pub text: String,
    pub accent: String,
    pub focus: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ThemeTokens {
    pub theme_schema: String,
    pub pack_id: String,
    pub theme_id: String,
    pub presentation_only: bool,
    pub mechanical_coverage: Vec<String>,
    pub accessible_description: String,
    pub non_color_cues: Vec<String>,
    pub palette: ThemePalette,
    pub border_style: String,
    pub heading_style: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPack {
    pub id: String,
    pub version: PackVersion,
    pub digest: Sha256Digest,
    pub dependencies: BTreeSet<String>,
    pub content_ids: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidationLimits {
    pub max_manifest_bytes: u64,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
    pub max_files: usize,
    pub max_depth: usize,
    pub max_entries: usize,
}

impl Default for ValidationLimits {
    fn default() -> Self {
        Self {
            max_manifest_bytes: 64 * 1024,
            max_file_bytes: 2 * 1024 * 1024,
            max_total_bytes: 16 * 1024 * 1024,
            max_files: 256,
            max_depth: 8,
            max_entries: 512,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PackValidationContext {
    pub limits: ValidationLimits,
    pub engine_rulesets: BTreeSet<String>,
    pub engine_capabilities: BTreeSet<String>,
    pub installed_packs: BTreeMap<String, InstalledPack>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationDisposition {
    Accepted,
    Quarantined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStage {
    Discovery,
    Manifest,
    Filesystem,
    Digest,
    Dependency,
    Capability,
    Reference,
    Provenance,
    Safety,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationFinding {
    pub severity: FindingSeverity,
    pub stage: ValidationStage,
    pub code: String,
    pub path: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidatedPackIdentity {
    pub id: String,
    pub version: PackVersion,
    pub digest: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationReport {
    pub disposition: ValidationDisposition,
    pub pack: Option<ValidatedPackIdentity>,
    pub findings: Vec<ValidationFinding>,
}

impl ValidationReport {
    fn new() -> Self {
        Self {
            disposition: ValidationDisposition::Accepted,
            pack: None,
            findings: Vec::new(),
        }
    }

    fn push(
        &mut self,
        severity: FindingSeverity,
        stage: ValidationStage,
        code: &'static str,
        path: Option<String>,
        message: impl Into<String>,
    ) {
        if severity == FindingSeverity::Error {
            self.disposition = ValidationDisposition::Quarantined;
        }
        self.findings.push(ValidationFinding {
            severity,
            stage,
            code: code.to_owned(),
            path,
            message: message.into(),
        });
    }

    pub fn activation_allowed(&self) -> bool {
        self.disposition == ValidationDisposition::Accepted
    }

    pub fn has_code(&self, code: &str) -> bool {
        self.findings.iter().any(|finding| finding.code == code)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveContentPack {
    identity: ValidatedPackIdentity,
    categories: BTreeSet<PackCategory>,
    active_content_ids: BTreeSet<String>,
    theme_tokens: Option<ThemeTokens>,
}

impl ActiveContentPack {
    pub fn identity(&self) -> &ValidatedPackIdentity {
        &self.identity
    }

    pub fn categories(&self) -> &BTreeSet<PackCategory> {
        &self.categories
    }

    pub fn active_content_ids(&self) -> &BTreeSet<String> {
        &self.active_content_ids
    }

    pub fn theme_tokens(&self) -> Option<&ThemeTokens> {
        self.theme_tokens.as_ref()
    }
}

/// Immutable process-wide view of the exact bundled content activated at boot.
/// There is intentionally no mutation or filesystem handle on this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveContentCatalog {
    ruleset_id: String,
    engine_capabilities: BTreeSet<String>,
    packs: BTreeMap<String, ActiveContentPack>,
    default_theme_pack_id: String,
}

impl ActiveContentCatalog {
    pub fn ruleset_id(&self) -> &str {
        &self.ruleset_id
    }

    pub fn engine_capabilities(&self) -> &BTreeSet<String> {
        &self.engine_capabilities
    }

    pub fn packs(&self) -> &BTreeMap<String, ActiveContentPack> {
        &self.packs
    }

    pub fn pack(&self, pack_id: &str) -> Option<&ActiveContentPack> {
        self.packs.get(pack_id)
    }

    pub fn default_theme(&self) -> &ActiveContentPack {
        self.packs
            .get(&self.default_theme_pack_id)
            .expect("catalog construction validates the default theme")
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ContentCatalogError {
    #[error("content pack root is unavailable or is not a real directory")]
    InvalidRoot,
    #[error("required content pack {pack_id} is missing")]
    RequiredPackMissing { pack_id: &'static str },
    #[error("required content pack {pack_id} was quarantined ({finding_codes})")]
    RequiredPackInvalid {
        pack_id: &'static str,
        finding_codes: String,
    },
    #[error("required content pack {pack_id} does not match its compiled exact pin")]
    ExactPinMismatch { pack_id: &'static str },
    #[error("validated content pack {pack_id} could not be activated safely")]
    ActivationReadFailed { pack_id: &'static str },
    #[error("content id {content_id} is declared by more than one required pack")]
    DuplicateContentId { content_id: String },
    #[error("default theme pack {pack_id} is not an allowlisted validated theme")]
    DefaultThemeUnavailable { pack_id: String },
}

#[derive(Debug, Clone, Copy)]
struct RequiredPackSpec {
    directory: &'static str,
    id: &'static str,
    version: &'static str,
    digest: &'static str,
    theme: bool,
}

const REQUIRED_PACKS: &[RequiredPackSpec] = &[
    RequiredPackSpec {
        directory: "core-mvp",
        id: CORE_CONTENT_PACK_ID,
        version: MVP_PACK_VERSION,
        digest: CORE_CONTENT_PACK_DIGEST,
        theme: false,
    },
    RequiredPackSpec {
        directory: "rainbound-borough",
        id: RAINBOUND_THEME_PACK_ID,
        version: MVP_PACK_VERSION,
        digest: RAINBOUND_THEME_PACK_DIGEST,
        theme: true,
    },
    RequiredPackSpec {
        directory: "emberline-archive",
        id: EMBERLINE_THEME_PACK_ID,
        version: MVP_PACK_VERSION,
        digest: EMBERLINE_THEME_PACK_DIGEST,
        theme: true,
    },
];

struct LoadedPack {
    active: ActiveContentPack,
    installed: InstalledPack,
}

/// Validates and activates only the three compiled private-MVP packs. Directory
/// discovery is intentionally absent: additional folders below `packs_root` are
/// ignored until code adds an exact ID/version/digest pin and capability review.
pub fn load_bundled_content_catalog(
    packs_root: &Path,
    default_theme_pack_id: &str,
) -> Result<ActiveContentCatalog, ContentCatalogError> {
    let root_metadata =
        fs::symlink_metadata(packs_root).map_err(|_| ContentCatalogError::InvalidRoot)?;
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        return Err(ContentCatalogError::InvalidRoot);
    }
    let canonical_root =
        fs::canonicalize(packs_root).map_err(|_| ContentCatalogError::InvalidRoot)?;
    let mut validation_context = PackValidationContext {
        engine_rulesets: BTreeSet::from([MVP_ENGINE_RULESET_ID.to_owned()]),
        engine_capabilities: MVP_ENGINE_CAPABILITIES
            .iter()
            .map(|capability| (*capability).to_owned())
            .collect(),
        ..PackValidationContext::default()
    };
    let mut packs = BTreeMap::new();
    let mut claimed_content_ids = BTreeSet::new();

    for spec in REQUIRED_PACKS {
        let configured_path = canonical_root.join(spec.directory);
        let metadata = fs::symlink_metadata(&configured_path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ContentCatalogError::RequiredPackMissing { pack_id: spec.id }
            } else {
                ContentCatalogError::RequiredPackInvalid {
                    pack_id: spec.id,
                    finding_codes: "pack_root_unreadable".to_owned(),
                }
            }
        })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(ContentCatalogError::RequiredPackInvalid {
                pack_id: spec.id,
                finding_codes: "pack_root_not_real_directory".to_owned(),
            });
        }
        let canonical_pack = fs::canonicalize(&configured_path).map_err(|_| {
            ContentCatalogError::RequiredPackInvalid {
                pack_id: spec.id,
                finding_codes: "pack_root_not_canonical".to_owned(),
            }
        })?;
        if canonical_pack.parent() != Some(canonical_root.as_path()) {
            return Err(ContentCatalogError::RequiredPackInvalid {
                pack_id: spec.id,
                finding_codes: "pack_root_outside_allowlist".to_owned(),
            });
        }

        let report = validate_pack(&canonical_pack, &validation_context);
        if !report.activation_allowed() {
            let finding_codes = report
                .findings
                .iter()
                .map(|finding| finding.code.as_str())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
                .join(",");
            return Err(ContentCatalogError::RequiredPackInvalid {
                pack_id: spec.id,
                finding_codes,
            });
        }
        let Some(identity) = report.pack else {
            return Err(ContentCatalogError::RequiredPackInvalid {
                pack_id: spec.id,
                finding_codes: "validated_identity_missing".to_owned(),
            });
        };
        if identity.id != spec.id
            || identity.version.as_str() != spec.version
            || identity.digest.as_str() != spec.digest
        {
            return Err(ContentCatalogError::ExactPinMismatch { pack_id: spec.id });
        }

        let loaded = activate_validated_pack(&canonical_pack, identity, *spec)?;
        for content_id in &loaded.installed.content_ids {
            if !claimed_content_ids.insert(content_id.clone()) {
                return Err(ContentCatalogError::DuplicateContentId {
                    content_id: content_id.clone(),
                });
            }
        }
        validation_context
            .installed_packs
            .insert(spec.id.to_owned(), loaded.installed);
        packs.insert(spec.id.to_owned(), loaded.active);
    }

    let default_theme = packs.get(default_theme_pack_id).filter(|pack| {
        pack.categories == BTreeSet::from([PackCategory::Theme]) && pack.theme_tokens.is_some()
    });
    if default_theme.is_none() {
        return Err(ContentCatalogError::DefaultThemeUnavailable {
            pack_id: default_theme_pack_id.to_owned(),
        });
    }

    Ok(ActiveContentCatalog {
        ruleset_id: MVP_ENGINE_RULESET_ID.to_owned(),
        engine_capabilities: validation_context.engine_capabilities,
        packs,
        default_theme_pack_id: default_theme_pack_id.to_owned(),
    })
}

fn activate_validated_pack(
    pack_root: &Path,
    identity: ValidatedPackIdentity,
    spec: RequiredPackSpec,
) -> Result<LoadedPack, ContentCatalogError> {
    let manifest_bytes = fs::read(pack_root.join(PACK_MANIFEST_FILE))
        .map_err(|_| ContentCatalogError::ActivationReadFailed { pack_id: spec.id })?;
    if manifest_bytes.len() as u64 > ValidationLimits::default().max_manifest_bytes {
        return Err(ContentCatalogError::ActivationReadFailed { pack_id: spec.id });
    }
    let manifest: PackManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|_| ContentCatalogError::ActivationReadFailed { pack_id: spec.id })?;
    if manifest.id != spec.id
        || manifest.version.as_str() != spec.version
        || manifest.digest.as_str() != spec.digest
        || compute_manifest_digest(&manifest) != manifest.digest
    {
        return Err(ContentCatalogError::ExactPinMismatch { pack_id: spec.id });
    }

    let mut content_ids = BTreeSet::new();
    let mut active_content_ids = BTreeSet::new();
    let mut theme_tokens = None;
    for file in &manifest.files {
        if !matches!(
            file.kind,
            ManifestFileKind::Content | ManifestFileKind::ThemeTokens
        ) {
            continue;
        }
        let bytes = read_catalog_payload(pack_root, file, spec.id)?;
        match file.kind {
            ManifestFileKind::Content => {
                let document: ContentDocument = serde_json::from_slice(&bytes)
                    .map_err(|_| ContentCatalogError::ActivationReadFailed { pack_id: spec.id })?;
                for entry in document.entries {
                    content_ids.insert(entry.id.clone());
                    if entry.availability == ContentAvailability::Active {
                        active_content_ids.insert(entry.id);
                    }
                }
            }
            ManifestFileKind::ThemeTokens => {
                let parsed: ThemeTokens = serde_json::from_slice(&bytes)
                    .map_err(|_| ContentCatalogError::ActivationReadFailed { pack_id: spec.id })?;
                if theme_tokens.replace(parsed).is_some() {
                    return Err(ContentCatalogError::ActivationReadFailed { pack_id: spec.id });
                }
            }
            ManifestFileKind::MechanicsTraceability | ManifestFileKind::Notice => unreachable!(),
        }
    }
    if spec.theme != theme_tokens.is_some() {
        return Err(ContentCatalogError::ActivationReadFailed { pack_id: spec.id });
    }

    let installed = InstalledPack {
        id: identity.id.clone(),
        version: identity.version.clone(),
        digest: identity.digest.clone(),
        dependencies: manifest
            .dependencies
            .iter()
            .map(|dependency| dependency.id.clone())
            .collect(),
        content_ids,
    };
    let active = ActiveContentPack {
        identity,
        categories: manifest.categories.into_iter().collect(),
        active_content_ids,
        theme_tokens,
    };
    Ok(LoadedPack { active, installed })
}

fn read_catalog_payload(
    pack_root: &Path,
    file: &ManifestFile,
    pack_id: &'static str,
) -> Result<Vec<u8>, ContentCatalogError> {
    let path = pack_root.join(&file.path);
    let metadata = fs::symlink_metadata(&path)
        .map_err(|_| ContentCatalogError::ActivationReadFailed { pack_id })?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > ValidationLimits::default().max_file_bytes
    {
        return Err(ContentCatalogError::ActivationReadFailed { pack_id });
    }
    let canonical = fs::canonicalize(&path)
        .map_err(|_| ContentCatalogError::ActivationReadFailed { pack_id })?;
    if !canonical.starts_with(pack_root) {
        return Err(ContentCatalogError::ActivationReadFailed { pack_id });
    }
    let bytes =
        fs::read(canonical).map_err(|_| ContentCatalogError::ActivationReadFailed { pack_id })?;
    if Sha256Digest::of_bytes(&bytes) != file.digest {
        return Err(ContentCatalogError::ActivationReadFailed { pack_id });
    }
    Ok(bytes)
}

#[derive(Debug, Default)]
struct Inventory {
    files: BTreeMap<String, PathBuf>,
    directories: BTreeSet<String>,
}

#[derive(Debug, Default)]
struct ParsedPack {
    entries: BTreeMap<String, ContentEntry>,
    entry_paths: BTreeMap<String, String>,
    traceability: Option<MechanicsTraceability>,
    themes: Vec<(String, ThemeTokens)>,
}

/// Performs all validation stages and returns a quarantine-style report. This
/// function has no write, network, template, HTML-rendering, or execution path.
pub fn validate_pack(pack_root: &Path, context: &PackValidationContext) -> ValidationReport {
    let mut report = ValidationReport::new();
    let canonical_root = match canonical_pack_root(pack_root, &mut report) {
        Some(root) => root,
        None => return report,
    };

    let manifest_path = canonical_root.join(PACK_MANIFEST_FILE);
    let manifest_bytes = match read_bounded(
        &manifest_path,
        context.limits.max_manifest_bytes,
        ValidationStage::Manifest,
        PACK_MANIFEST_FILE,
        &mut report,
    ) {
        Some(bytes) => bytes,
        None => return report,
    };
    let manifest: PackManifest = match serde_json::from_slice(&manifest_bytes) {
        Ok(manifest) => manifest,
        Err(error) => {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Manifest,
                "manifest_schema_invalid",
                Some(PACK_MANIFEST_FILE.to_owned()),
                format!("manifest is not strict content-pack/v1 JSON: {error}"),
            );
            return report;
        }
    };
    report.pack = Some(ValidatedPackIdentity {
        id: manifest.id.clone(),
        version: manifest.version.clone(),
        digest: manifest.digest.clone(),
    });

    validate_manifest(&manifest, &mut report);
    let inventory = inventory_pack(&canonical_root, context.limits, &mut report);
    validate_inventory(&manifest, &inventory, &mut report);
    validate_digests(&manifest, &inventory, context.limits, &mut report);

    let payloads = read_manifest_payloads(&manifest, &inventory, context.limits, &mut report);
    let mut parsed = parse_payloads(&manifest, &payloads, context, &mut report);
    let provenance = parse_provenance(&manifest, &inventory, context.limits, &mut report);

    validate_dependencies(&manifest, context, &mut report);
    validate_capabilities(&manifest, &parsed, context, &mut report);
    validate_references(&manifest, &mut parsed, context, &mut report);
    validate_traceability(&manifest, &parsed, &mut report);
    validate_provenance(&manifest, &parsed, provenance.as_ref(), &mut report);
    validate_safety(&inventory, context.limits, &mut report);

    report
}

fn canonical_pack_root(root: &Path, report: &mut ValidationReport) -> Option<PathBuf> {
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) => {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Discovery,
                "pack_root_unreadable",
                None,
                format!("pack root cannot be inspected: {error}"),
            );
            return None;
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        report.push(
            FindingSeverity::Error,
            ValidationStage::Discovery,
            "pack_root_not_real_directory",
            None,
            "pack root must be a real directory, not a file or symlink",
        );
        return None;
    }
    match fs::canonicalize(root) {
        Ok(root) => Some(root),
        Err(error) => {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Discovery,
                "pack_root_not_canonical",
                None,
                format!("pack root cannot be canonicalized: {error}"),
            );
            None
        }
    }
}

fn read_bounded(
    path: &Path,
    maximum: u64,
    stage: ValidationStage,
    display_path: &str,
    report: &mut ValidationReport,
) -> Option<Vec<u8>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            report.push(
                FindingSeverity::Error,
                stage,
                "file_unreadable",
                Some(display_path.to_owned()),
                format!("file cannot be inspected: {error}"),
            );
            return None;
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        report.push(
            FindingSeverity::Error,
            stage,
            "file_not_regular",
            Some(display_path.to_owned()),
            "file must be a regular file and may not be a symlink",
        );
        return None;
    }
    if metadata.len() > maximum {
        report.push(
            FindingSeverity::Error,
            stage,
            "file_too_large",
            Some(display_path.to_owned()),
            format!("file is {} bytes; limit is {maximum}", metadata.len()),
        );
        return None;
    }
    match fs::read(path) {
        Ok(bytes) if bytes.len() as u64 <= maximum => Some(bytes),
        Ok(bytes) => {
            report.push(
                FindingSeverity::Error,
                stage,
                "file_grew_during_read",
                Some(display_path.to_owned()),
                format!("file grew to {} bytes while being read", bytes.len()),
            );
            None
        }
        Err(error) => {
            report.push(
                FindingSeverity::Error,
                stage,
                "file_read_failed",
                Some(display_path.to_owned()),
                format!("file cannot be read: {error}"),
            );
            None
        }
    }
}

fn validate_manifest(manifest: &PackManifest, report: &mut ValidationReport) {
    if manifest.pack_schema != CONTENT_PACK_SCHEMA {
        report.push(
            FindingSeverity::Error,
            ValidationStage::Manifest,
            "unsupported_pack_schema",
            Some(PACK_MANIFEST_FILE.to_owned()),
            format!("expected {CONTENT_PACK_SCHEMA}"),
        );
    }
    if !is_valid_pack_id(&manifest.id) {
        manifest_error(
            report,
            "invalid_pack_id",
            "pack id must be a lowercase reverse-domain id",
        );
    }
    if !is_bounded_text(&manifest.display_name, 1, 120) {
        manifest_error(
            report,
            "invalid_display_name",
            "display name must contain 1 to 120 characters",
        );
    }
    validate_nonempty_unique(&manifest.categories, "categories", report);
    validate_tokens(
        &manifest.compatible_rulesets,
        is_valid_ruleset_id,
        "compatible_rulesets",
        report,
    );
    validate_tokens(
        &manifest.required_engine_capabilities,
        is_valid_capability_id,
        "required_engine_capabilities",
        report,
    );

    let mut dependency_ids = BTreeSet::new();
    for dependency in &manifest.dependencies {
        if !is_valid_pack_id(&dependency.id)
            || dependency.id == manifest.id
            || !dependency_ids.insert(dependency.id.as_str())
        {
            manifest_error(
                report,
                "invalid_dependency",
                "dependencies need unique non-self pack ids and exact version/digest pins",
            );
        }
    }
    if !is_safe_relative_path(&manifest.license.notice_path)
        || manifest.license.license_id.trim().is_empty()
        || manifest
            .license
            .license_url
            .as_ref()
            .is_some_and(|url| !is_bounded_text(url, 1, 500))
        || manifest.license.allowed_content_license_ids.is_empty()
        || has_duplicates(&manifest.license.allowed_content_license_ids)
        || manifest
            .license
            .allowed_content_license_ids
            .iter()
            .any(|license| license.trim().is_empty())
    {
        manifest_error(
            report,
            "invalid_license",
            "pack license declaration is incomplete",
        );
    }
    if manifest.provenance_manifest != "provenance.json" {
        manifest_error(
            report,
            "invalid_provenance_path",
            "provenance manifest must be the root file provenance.json",
        );
    }
    if manifest.content_roots.is_empty() || has_duplicates(&manifest.content_roots) {
        manifest_error(
            report,
            "invalid_content_roots",
            "content roots must be non-empty and unique",
        );
    }
    for root in &manifest.content_roots {
        if !ALLOWLISTED_CONTENT_ROOTS.contains(&root.as_str()) {
            manifest_error(
                report,
                "content_root_not_allowlisted",
                "content roots must use the built-in data-only allowlist",
            );
        }
    }

    let mut paths = BTreeSet::new();
    for file in &manifest.files {
        if !is_safe_relative_path(&file.path)
            || !paths.insert(file.path.as_str())
            || !is_valid_namespaced_id(&file.provenance_key)
        {
            manifest_error(
                report,
                "invalid_manifest_file",
                "manifest file paths and provenance keys must be safe and unique",
            );
            continue;
        }
        let Some(root) = file.path.split('/').next() else {
            continue;
        };
        if !manifest.content_roots.iter().any(|allowed| allowed == root) {
            manifest_error(
                report,
                "file_outside_content_roots",
                "every payload must be beneath a declared content root",
            );
        }
        let extension = Path::new(&file.path)
            .extension()
            .and_then(|value| value.to_str());
        let expected = match file.kind {
            ManifestFileKind::Notice => "txt",
            ManifestFileKind::Content
            | ManifestFileKind::MechanicsTraceability
            | ManifestFileKind::ThemeTokens => "json",
        };
        if extension != Some(expected) {
            manifest_error(
                report,
                "file_type_not_allowlisted",
                "payloads are limited to declared JSON data and plain-text notices",
            );
        }
    }
    if !paths.contains(manifest.license.notice_path.as_str()) {
        manifest_error(
            report,
            "notice_not_indexed",
            "license notice path must be present in the immutable file index",
        );
    }

    let expected_digest = compute_manifest_digest(manifest);
    if expected_digest != manifest.digest {
        report.push(
            FindingSeverity::Error,
            ValidationStage::Digest,
            "manifest_digest_mismatch",
            Some(PACK_MANIFEST_FILE.to_owned()),
            format!("computed {expected_digest}, declared {}", manifest.digest),
        );
    }
}

fn manifest_error(report: &mut ValidationReport, code: &'static str, message: &'static str) {
    report.push(
        FindingSeverity::Error,
        ValidationStage::Manifest,
        code,
        Some(PACK_MANIFEST_FILE.to_owned()),
        message,
    );
}

fn inventory_pack(
    root: &Path,
    limits: ValidationLimits,
    report: &mut ValidationReport,
) -> Inventory {
    let mut inventory = Inventory::default();
    let mut stack = vec![root.to_owned()];
    let mut total_bytes = 0_u64;
    while let Some(directory) = stack.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                report.push(
                    FindingSeverity::Error,
                    ValidationStage::Filesystem,
                    "directory_unreadable",
                    relative_display(root, &directory),
                    format!("directory cannot be enumerated: {error}"),
                );
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    report.push(
                        FindingSeverity::Error,
                        ValidationStage::Filesystem,
                        "directory_entry_unreadable",
                        relative_display(root, &directory),
                        format!("directory entry cannot be inspected: {error}"),
                    );
                    continue;
                }
            };
            let path = entry.path();
            let Some(relative) = relative_display(root, &path) else {
                report.push(
                    FindingSeverity::Error,
                    ValidationStage::Filesystem,
                    "non_utf8_path",
                    None,
                    "pack paths must be portable UTF-8",
                );
                continue;
            };
            if !is_safe_relative_path(&relative) {
                report.push(
                    FindingSeverity::Error,
                    ValidationStage::Filesystem,
                    "unsafe_filesystem_path",
                    Some(relative),
                    "pack path is not a canonical portable relative path",
                );
                continue;
            }
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) => {
                    report.push(
                        FindingSeverity::Error,
                        ValidationStage::Filesystem,
                        "path_unreadable",
                        Some(relative),
                        format!("path metadata cannot be read: {error}"),
                    );
                    continue;
                }
            };
            if metadata.file_type().is_symlink() {
                report.push(
                    FindingSeverity::Error,
                    ValidationStage::Filesystem,
                    "symlink_forbidden",
                    Some(relative),
                    "symlinks are never followed or accepted in a pack",
                );
                continue;
            }
            let canonical = match fs::canonicalize(&path) {
                Ok(canonical) if canonical.starts_with(root) => canonical,
                Ok(_) => {
                    report.push(
                        FindingSeverity::Error,
                        ValidationStage::Filesystem,
                        "canonical_path_escape",
                        Some(relative),
                        "canonical path escapes the pack root",
                    );
                    continue;
                }
                Err(error) => {
                    report.push(
                        FindingSeverity::Error,
                        ValidationStage::Filesystem,
                        "canonical_path_failed",
                        Some(relative),
                        format!("path cannot be canonicalized: {error}"),
                    );
                    continue;
                }
            };
            let depth = Path::new(&relative).components().count();
            if depth > limits.max_depth {
                report.push(
                    FindingSeverity::Error,
                    ValidationStage::Filesystem,
                    "path_depth_exceeded",
                    Some(relative.clone()),
                    format!("path depth {depth} exceeds limit {}", limits.max_depth),
                );
            }
            if metadata.is_dir() {
                inventory.directories.insert(relative);
                stack.push(canonical);
            } else if metadata.is_file() {
                if metadata.len() > limits.max_file_bytes {
                    report.push(
                        FindingSeverity::Error,
                        ValidationStage::Filesystem,
                        "file_size_exceeded",
                        Some(relative.clone()),
                        format!(
                            "file is {} bytes; limit is {}",
                            metadata.len(),
                            limits.max_file_bytes
                        ),
                    );
                }
                total_bytes = total_bytes.saturating_add(metadata.len());
                inventory.files.insert(relative, canonical);
            } else {
                report.push(
                    FindingSeverity::Error,
                    ValidationStage::Filesystem,
                    "special_file_forbidden",
                    Some(relative),
                    "only regular files and directories are accepted",
                );
            }
        }
    }
    if inventory.files.len() > limits.max_files {
        report.push(
            FindingSeverity::Error,
            ValidationStage::Filesystem,
            "file_count_exceeded",
            None,
            format!(
                "pack has {} files; limit is {}",
                inventory.files.len(),
                limits.max_files
            ),
        );
    }
    if total_bytes > limits.max_total_bytes {
        report.push(
            FindingSeverity::Error,
            ValidationStage::Filesystem,
            "pack_size_exceeded",
            None,
            format!(
                "pack is {total_bytes} bytes; limit is {}",
                limits.max_total_bytes
            ),
        );
    }
    inventory
}

fn validate_inventory(
    manifest: &PackManifest,
    inventory: &Inventory,
    report: &mut ValidationReport,
) {
    let expected_files = manifest
        .files
        .iter()
        .map(|file| file.path.clone())
        .chain([
            PACK_MANIFEST_FILE.to_owned(),
            manifest.provenance_manifest.clone(),
        ])
        .collect::<BTreeSet<_>>();
    for expected in &expected_files {
        if !inventory.files.contains_key(expected) {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Filesystem,
                "indexed_file_missing",
                Some(expected.clone()),
                "immutable file index names a file that is absent",
            );
        }
    }
    for actual in inventory.files.keys() {
        if !expected_files.contains(actual) {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Filesystem,
                "unindexed_file",
                Some(actual.clone()),
                "all pack files must be present in the immutable index",
            );
        }
    }
    for root in &manifest.content_roots {
        if !inventory.directories.contains(root) {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Filesystem,
                "content_root_missing",
                Some(root.clone()),
                "declared content root is absent",
            );
        }
    }
    for directory in &inventory.directories {
        let top = directory.split('/').next().unwrap_or_default();
        if !manifest.content_roots.iter().any(|root| root == top) {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Filesystem,
                "undeclared_content_root",
                Some(directory.clone()),
                "directory is outside the manifest's allowlisted content roots",
            );
        }
    }
}

fn validate_digests(
    manifest: &PackManifest,
    inventory: &Inventory,
    limits: ValidationLimits,
    report: &mut ValidationReport,
) {
    for file in &manifest.files {
        let Some(path) = inventory.files.get(&file.path) else {
            continue;
        };
        let Some(bytes) = read_bounded(
            path,
            limits.max_file_bytes,
            ValidationStage::Digest,
            &file.path,
            report,
        ) else {
            continue;
        };
        let actual = Sha256Digest::of_bytes(&bytes);
        if actual != file.digest {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Digest,
                "payload_digest_mismatch",
                Some(file.path.clone()),
                format!("computed {actual}, declared {}", file.digest),
            );
        }
    }
    if let Some(path) = inventory.files.get(&manifest.provenance_manifest)
        && let Some(bytes) = read_bounded(
            path,
            limits.max_file_bytes,
            ValidationStage::Digest,
            &manifest.provenance_manifest,
            report,
        )
    {
        let actual = Sha256Digest::of_bytes(&bytes);
        if actual != manifest.provenance_digest {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Digest,
                "provenance_digest_mismatch",
                Some(manifest.provenance_manifest.clone()),
                format!("computed {actual}, declared {}", manifest.provenance_digest),
            );
        }
    }
}

fn read_manifest_payloads(
    manifest: &PackManifest,
    inventory: &Inventory,
    limits: ValidationLimits,
    report: &mut ValidationReport,
) -> BTreeMap<String, Vec<u8>> {
    manifest
        .files
        .iter()
        .filter_map(|file| {
            let path = inventory.files.get(&file.path)?;
            read_bounded(
                path,
                limits.max_file_bytes,
                ValidationStage::Filesystem,
                &file.path,
                report,
            )
            .map(|bytes| (file.path.clone(), bytes))
        })
        .collect()
}

fn parse_payloads(
    manifest: &PackManifest,
    payloads: &BTreeMap<String, Vec<u8>>,
    context: &PackValidationContext,
    report: &mut ValidationReport,
) -> ParsedPack {
    let mut parsed = ParsedPack::default();
    for file in &manifest.files {
        let Some(bytes) = payloads.get(&file.path) else {
            continue;
        };
        match file.kind {
            ManifestFileKind::Content => {
                let document: ContentDocument = match strict_json(bytes, &file.path, report) {
                    Some(document) => document,
                    None => continue,
                };
                validate_content_document(
                    &document,
                    &file.path,
                    context.limits.max_entries,
                    report,
                );
                for entry in document.entries {
                    if parsed.entries.contains_key(&entry.id) {
                        report.push(
                            FindingSeverity::Error,
                            ValidationStage::Reference,
                            "duplicate_content_id",
                            Some(file.path.clone()),
                            format!("content id {} appears more than once", entry.id),
                        );
                    } else {
                        parsed
                            .entry_paths
                            .insert(entry.id.clone(), file.path.clone());
                        parsed.entries.insert(entry.id.clone(), entry);
                    }
                }
            }
            ManifestFileKind::MechanicsTraceability => {
                let traceability: MechanicsTraceability =
                    match strict_json(bytes, &file.path, report) {
                        Some(traceability) => traceability,
                        None => continue,
                    };
                if parsed.traceability.replace(traceability).is_some() {
                    report.push(
                        FindingSeverity::Error,
                        ValidationStage::Reference,
                        "multiple_traceability_documents",
                        Some(file.path.clone()),
                        "a pack has exactly one mechanics traceability document",
                    );
                }
            }
            ManifestFileKind::ThemeTokens => {
                let theme: ThemeTokens = match strict_json(bytes, &file.path, report) {
                    Some(theme) => theme,
                    None => continue,
                };
                parsed.themes.push((file.path.clone(), theme));
            }
            ManifestFileKind::Notice => {
                if std::str::from_utf8(bytes).is_err() {
                    report.push(
                        FindingSeverity::Error,
                        ValidationStage::Safety,
                        "notice_not_utf8",
                        Some(file.path.clone()),
                        "plain-text notices must be UTF-8",
                    );
                }
            }
        }
    }
    parsed
}

fn strict_json<T: for<'de> Deserialize<'de>>(
    bytes: &[u8],
    path: &str,
    report: &mut ValidationReport,
) -> Option<T> {
    match serde_json::from_slice(bytes) {
        Ok(value) => Some(value),
        Err(error) => {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Manifest,
                "content_schema_invalid",
                Some(path.to_owned()),
                format!("JSON does not match its strict declared schema: {error}"),
            );
            None
        }
    }
}

fn validate_content_document(
    document: &ContentDocument,
    path: &str,
    maximum_entries: usize,
    report: &mut ValidationReport,
) {
    if document.content_schema != CONTENT_DOCUMENT_SCHEMA
        || !is_valid_namespaced_id(&document.document_id)
        || document.entries.is_empty()
        || document.entries.len() > maximum_entries
    {
        report.push(
            FindingSeverity::Error,
            ValidationStage::Manifest,
            "content_document_invalid",
            Some(path.to_owned()),
            "content document schema, id, or entry count is invalid",
        );
    }
    let mut ids = BTreeSet::new();
    for entry in &document.entries {
        if entry.schema_version != 1
            || !is_valid_namespaced_id(&entry.id)
            || !ids.insert(entry.id.as_str())
            || !is_bounded_text(&entry.display_name, 1, 120)
            || !is_bounded_text(&entry.description, 1, 500)
            || !is_valid_ruleset_id(&entry.ruleset_id)
            || !is_valid_namespaced_id(&entry.source_key)
            || !is_valid_namespaced_id(&entry.provenance_key)
            || entry.effects.is_empty()
            || has_duplicates(&entry.references)
            || has_duplicates(&entry.required_engine_capabilities)
            || entry
                .references
                .iter()
                .any(|reference| !is_valid_namespaced_id(reference))
            || entry
                .required_engine_capabilities
                .iter()
                .any(|capability| !is_valid_capability_id(capability))
        {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Manifest,
                "content_entry_invalid",
                Some(path.to_owned()),
                format!("content entry {} has invalid or duplicate fields", entry.id),
            );
        }
        for effect in &entry.effects {
            match effect {
                TypedEffect::EnginePolicy { capability } => {
                    if !entry.required_engine_capabilities.contains(capability) {
                        report.push(
                            FindingSeverity::Error,
                            ValidationStage::Capability,
                            "effect_capability_undeclared",
                            Some(path.to_owned()),
                            format!("{} uses undeclared capability {capability}", entry.id),
                        );
                    }
                }
                TypedEffect::Composes { content_ids } | TypedEffect::Offers { content_ids } => {
                    if content_ids.is_empty()
                        || has_duplicates(content_ids)
                        || content_ids.iter().any(|id| !entry.references.contains(id))
                    {
                        report.push(
                            FindingSeverity::Error,
                            ValidationStage::Reference,
                            "effect_reference_undeclared",
                            Some(path.to_owned()),
                            format!("{} has effect references absent from references", entry.id),
                        );
                    }
                }
                TypedEffect::TransitionsTo { content_id } => {
                    if !entry.references.contains(content_id) {
                        report.push(
                            FindingSeverity::Error,
                            ValidationStage::Reference,
                            "effect_reference_undeclared",
                            Some(path.to_owned()),
                            format!("{} transition is absent from references", entry.id),
                        );
                    }
                }
            }
        }
    }
}

fn parse_provenance(
    manifest: &PackManifest,
    inventory: &Inventory,
    limits: ValidationLimits,
    report: &mut ValidationReport,
) -> Option<ProvenanceManifest> {
    let path = inventory.files.get(&manifest.provenance_manifest)?;
    let bytes = read_bounded(
        path,
        limits.max_file_bytes,
        ValidationStage::Provenance,
        &manifest.provenance_manifest,
        report,
    )?;
    strict_json(&bytes, &manifest.provenance_manifest, report)
}

fn validate_dependencies(
    manifest: &PackManifest,
    context: &PackValidationContext,
    report: &mut ValidationReport,
) {
    for dependency in &manifest.dependencies {
        let Some(installed) = context.installed_packs.get(&dependency.id) else {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Dependency,
                "dependency_missing",
                Some(PACK_MANIFEST_FILE.to_owned()),
                format!(
                    "exact dependency {} {} is not installed",
                    dependency.id, dependency.version
                ),
            );
            continue;
        };
        if installed.id != dependency.id
            || installed.version != dependency.version
            || installed.digest != dependency.digest
        {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Dependency,
                "dependency_pin_mismatch",
                Some(PACK_MANIFEST_FILE.to_owned()),
                format!(
                    "installed dependency {} does not match its exact pin",
                    dependency.id
                ),
            );
        }
        if dependency_reaches(&dependency.id, &manifest.id, &context.installed_packs) {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Dependency,
                "dependency_cycle",
                Some(PACK_MANIFEST_FILE.to_owned()),
                format!(
                    "dependency {} reaches this pack and creates a cycle",
                    dependency.id
                ),
            );
        }
    }
}

fn dependency_reaches(
    start: &str,
    target: &str,
    installed: &BTreeMap<String, InstalledPack>,
) -> bool {
    let mut pending = vec![start];
    let mut visited = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if id == target {
            return true;
        }
        if !visited.insert(id) {
            continue;
        }
        if let Some(pack) = installed.get(id) {
            pending.extend(pack.dependencies.iter().map(String::as_str));
        }
    }
    false
}

fn validate_capabilities(
    manifest: &PackManifest,
    parsed: &ParsedPack,
    context: &PackValidationContext,
    report: &mut ValidationReport,
) {
    if !manifest
        .compatible_rulesets
        .iter()
        .any(|ruleset| context.engine_rulesets.contains(ruleset))
    {
        report.push(
            FindingSeverity::Error,
            ValidationStage::Capability,
            "engine_ruleset_missing",
            Some(PACK_MANIFEST_FILE.to_owned()),
            "engine does not declare a ruleset compatible with this pack",
        );
    }
    for capability in &manifest.required_engine_capabilities {
        if !context.engine_capabilities.contains(capability) {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Capability,
                "engine_capability_missing",
                Some(PACK_MANIFEST_FILE.to_owned()),
                format!("engine does not declare required capability {capability}"),
            );
        }
    }
    for entry in parsed.entries.values() {
        if !manifest.compatible_rulesets.contains(&entry.ruleset_id) {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Capability,
                "content_ruleset_undeclared",
                parsed.entry_paths.get(&entry.id).cloned(),
                format!(
                    "content {} uses ruleset {} absent from the manifest",
                    entry.id, entry.ruleset_id
                ),
            );
        }
        if entry.availability == ContentAvailability::Planned {
            continue;
        }
        if !context.engine_rulesets.contains(&entry.ruleset_id) {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Capability,
                "active_ruleset_unsupported",
                parsed.entry_paths.get(&entry.id).cloned(),
                format!(
                    "active content {} requires unsupported ruleset {}",
                    entry.id, entry.ruleset_id
                ),
            );
        }
        for capability in &entry.required_engine_capabilities {
            if !manifest.required_engine_capabilities.contains(capability) {
                report.push(
                    FindingSeverity::Error,
                    ValidationStage::Capability,
                    "active_capability_not_in_manifest",
                    parsed.entry_paths.get(&entry.id).cloned(),
                    format!(
                        "active content {} requires undeclared {capability}",
                        entry.id
                    ),
                );
            }
            if !context.engine_capabilities.contains(capability) {
                report.push(
                    FindingSeverity::Error,
                    ValidationStage::Capability,
                    "active_content_unsupported",
                    parsed.entry_paths.get(&entry.id).cloned(),
                    format!(
                        "active content {} requires unsupported {capability}",
                        entry.id
                    ),
                );
            }
        }
    }
    for (path, theme) in &parsed.themes {
        if theme.theme_schema != THEME_TOKENS_SCHEMA
            || theme.pack_id != manifest.id
            || !is_valid_namespaced_id(&theme.theme_id)
            || !theme.presentation_only
            || theme.mechanical_coverage != manifest.required_engine_capabilities
            || !is_bounded_text(&theme.accessible_description, 1, 500)
            || theme.non_color_cues.len() < 2
            || has_duplicates(&theme.non_color_cues)
            || theme
                .non_color_cues
                .iter()
                .any(|cue| !is_bounded_text(cue, 1, 200))
            || !valid_hex_color(&theme.palette.background)
            || !valid_hex_color(&theme.palette.surface)
            || !valid_hex_color(&theme.palette.text)
            || !valid_hex_color(&theme.palette.accent)
            || !valid_hex_color(&theme.palette.focus)
            || !is_bounded_text(&theme.border_style, 1, 100)
            || !is_bounded_text(&theme.heading_style, 1, 100)
        {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Capability,
                "theme_contract_invalid",
                Some(path.clone()),
                "theme must be presentation-only, accessible without color, and exactly match its declared mechanical coverage",
            );
        }
    }
    if !parsed.themes.is_empty()
        && (manifest.categories != [PackCategory::Theme] || !parsed.entries.is_empty())
    {
        report.push(
            FindingSeverity::Error,
            ValidationStage::Capability,
            "theme_pack_has_mechanical_content",
            Some(PACK_MANIFEST_FILE.to_owned()),
            "a presentation-only theme pack cannot contain rule-bearing content",
        );
    }
}

fn validate_references(
    manifest: &PackManifest,
    parsed: &mut ParsedPack,
    context: &PackValidationContext,
    report: &mut ValidationReport,
) {
    let dependency_ids = manifest
        .dependencies
        .iter()
        .filter_map(|dependency| context.installed_packs.get(&dependency.id))
        .flat_map(|pack| pack.content_ids.iter().cloned())
        .collect::<BTreeSet<_>>();
    for entry in parsed.entries.values() {
        for reference in &entry.references {
            if !parsed.entries.contains_key(reference) && !dependency_ids.contains(reference) {
                report.push(
                    FindingSeverity::Error,
                    ValidationStage::Reference,
                    "dangling_content_reference",
                    parsed.entry_paths.get(&entry.id).cloned(),
                    format!("{} refers to unknown content {reference}", entry.id),
                );
            }
        }
    }

    let local_edges = parsed
        .entries
        .iter()
        .map(|(id, entry)| {
            (
                id.clone(),
                entry
                    .references
                    .iter()
                    .filter(|reference| parsed.entries.contains_key(*reference))
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    if let Some(cycle) = find_reference_cycle(&local_edges) {
        report.push(
            FindingSeverity::Error,
            ValidationStage::Reference,
            "content_reference_cycle",
            None,
            format!("content references form a cycle: {}", cycle.join(" -> ")),
        );
    }
}

fn find_reference_cycle(edges: &BTreeMap<String, Vec<String>>) -> Option<Vec<String>> {
    fn visit(
        id: &str,
        edges: &BTreeMap<String, Vec<String>>,
        visiting: &mut Vec<String>,
        complete: &mut BTreeSet<String>,
    ) -> Option<Vec<String>> {
        if let Some(index) = visiting.iter().position(|candidate| candidate == id) {
            let mut cycle = visiting[index..].to_vec();
            cycle.push(id.to_owned());
            return Some(cycle);
        }
        if complete.contains(id) {
            return None;
        }
        visiting.push(id.to_owned());
        for next in edges.get(id).into_iter().flatten() {
            if let Some(cycle) = visit(next, edges, visiting, complete) {
                return Some(cycle);
            }
        }
        visiting.pop();
        complete.insert(id.to_owned());
        None
    }

    let mut complete = BTreeSet::new();
    for id in edges.keys() {
        if let Some(cycle) = visit(id, edges, &mut Vec::new(), &mut complete) {
            return Some(cycle);
        }
    }
    None
}

fn validate_traceability(
    manifest: &PackManifest,
    parsed: &ParsedPack,
    report: &mut ValidationReport,
) {
    if parsed.entries.is_empty() {
        return;
    }
    let Some(traceability) = &parsed.traceability else {
        report.push(
            FindingSeverity::Error,
            ValidationStage::Reference,
            "traceability_missing",
            None,
            "rule-bearing content requires one mechanics traceability document",
        );
        return;
    };
    if traceability.traceability_schema != TRACEABILITY_SCHEMA
        || traceability.pack_id != manifest.id
        || traceability.pack_version != manifest.version
        || traceability.entries.len() > 512
    {
        report.push(
            FindingSeverity::Error,
            ValidationStage::Reference,
            "traceability_header_invalid",
            None,
            "traceability schema and exact pack pin must match the manifest",
        );
    }
    let mut traces = BTreeMap::new();
    for trace in &traceability.entries {
        if !is_valid_namespaced_id(&trace.mechanic_id)
            || traces.insert(trace.mechanic_id.as_str(), trace).is_some()
            || !is_valid_namespaced_id(&trace.source_key)
            || !is_bounded_text(&trace.source_location, 1, 500)
            || !is_valid_namespaced_id(&trace.provenance_key)
            || !is_bounded_text(&trace.modification_note, 1, 500)
            || has_duplicates(&trace.required_engine_capabilities)
            || has_duplicates(&trace.consuming_content)
        {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Reference,
                "traceability_entry_invalid",
                None,
                format!("traceability entry {} is malformed", trace.mechanic_id),
            );
        }
        for consumer in &trace.consuming_content {
            if !parsed.entries.contains_key(consumer) {
                report.push(
                    FindingSeverity::Error,
                    ValidationStage::Reference,
                    "traceability_consumer_missing",
                    None,
                    format!("{} names unknown consumer {consumer}", trace.mechanic_id),
                );
            }
        }
    }
    for entry in parsed.entries.values() {
        let Some(trace) = traces.get(entry.id.as_str()) else {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Reference,
                "content_trace_missing",
                parsed.entry_paths.get(&entry.id).cloned(),
                format!("{} has no traceability row", entry.id),
            );
            continue;
        };
        if trace.availability != entry.availability
            || trace.source_key != entry.source_key
            || trace.license_class != entry.license_class
            || trace.provenance_key != entry.provenance_key
            || trace.required_engine_capabilities != entry.required_engine_capabilities
        {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Reference,
                "traceability_content_mismatch",
                parsed.entry_paths.get(&entry.id).cloned(),
                format!("{} trace does not match its content declaration", entry.id),
            );
        }
        if entry.availability == ContentAvailability::Active
            && (trace.implementation_symbols.is_empty()
                || trace.test_ids.is_empty()
                || trace.consuming_content.is_empty())
        {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Reference,
                "active_trace_incomplete",
                parsed.entry_paths.get(&entry.id).cloned(),
                format!(
                    "active mechanic {} lacks implementation, tests, or consumers",
                    entry.id
                ),
            );
        }
        if entry.availability == ContentAvailability::Planned
            && (trace.implementation_symbols.is_empty() || trace.test_ids.is_empty())
        {
            report.push(
                FindingSeverity::Warning,
                ValidationStage::Reference,
                "planned_content_inert",
                parsed.entry_paths.get(&entry.id).cloned(),
                format!("planned mechanic {} remains unavailable", entry.id),
            );
        }
    }
}

fn validate_provenance(
    manifest: &PackManifest,
    parsed: &ParsedPack,
    provenance: Option<&ProvenanceManifest>,
    report: &mut ValidationReport,
) {
    let Some(provenance) = provenance else {
        report.push(
            FindingSeverity::Error,
            ValidationStage::Provenance,
            "provenance_missing",
            Some(manifest.provenance_manifest.clone()),
            "pack has no readable provenance manifest",
        );
        return;
    };
    if provenance.provenance_schema != PROVENANCE_SCHEMA
        || provenance.pack_id != manifest.id
        || provenance.pack_version != manifest.version
    {
        report.push(
            FindingSeverity::Error,
            ValidationStage::Provenance,
            "provenance_header_invalid",
            Some(manifest.provenance_manifest.clone()),
            "provenance schema and exact pack pin must match the manifest",
        );
    }
    let mut entries = BTreeMap::new();
    let mut covered_paths = BTreeSet::new();
    for entry in &provenance.entries {
        if !is_valid_namespaced_id(&entry.provenance_key)
            || entries
                .insert(entry.provenance_key.as_str(), entry)
                .is_some()
            || !is_bounded_text(&entry.title, 1, 200)
            || !is_bounded_text(&entry.creator, 1, 200)
            || !is_bounded_text(&entry.rightsholder, 1, 200)
            || !is_bounded_text(&entry.source_locator, 1, 500)
            || !is_bounded_text(&entry.license_id, 1, 100)
            || entry
                .license_url
                .as_ref()
                .is_some_and(|url| !is_bounded_text(url, 1, 500))
            || !is_bounded_text(&entry.required_notice, 1, 500)
            || !is_bounded_text(&entry.modification_note, 1, 500)
            || !is_iso_date(&entry.created_or_retrieved_at)
            || !is_bounded_text(&entry.reviewer, 1, 100)
            || entry.pack_id != manifest.id
            || entry.pack_version != manifest.version
            || has_duplicates(&entry.subject_ids)
            || has_duplicates(&entry.ruleset_ids)
            || entry.ruleset_ids.is_empty()
            || entry
                .ruleset_ids
                .iter()
                .any(|ruleset| !is_valid_ruleset_id(ruleset))
            || !manifest
                .license
                .allowed_content_license_ids
                .contains(&entry.license_id)
        {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Provenance,
                "provenance_entry_invalid",
                entry.path.clone(),
                format!(
                    "provenance entry {} is incomplete or inconsistent",
                    entry.provenance_key
                ),
            );
        }
        match (&entry.path, &entry.digest) {
            (Some(path), Some(_)) if is_safe_relative_path(path) => {
                if !covered_paths.insert(path.as_str()) {
                    report.push(
                        FindingSeverity::Error,
                        ValidationStage::Provenance,
                        "duplicate_file_provenance",
                        Some(path.clone()),
                        "a file may have only one provenance record",
                    );
                }
            }
            (None, None) if !entry.subject_ids.is_empty() => {}
            _ => report.push(
                FindingSeverity::Error,
                ValidationStage::Provenance,
                "provenance_subject_invalid",
                entry.path.clone(),
                "provenance must identify either a hashed file or one or more content subjects",
            ),
        }
        for subject in &entry.subject_ids {
            if !parsed.entries.contains_key(subject) {
                report.push(
                    FindingSeverity::Error,
                    ValidationStage::Provenance,
                    "provenance_subject_missing",
                    None,
                    format!("provenance names unknown content subject {subject}"),
                );
            }
        }
        if entry.origin == ProvenanceOrigin::Srd5_1Cc
            && (entry.license_id != "CC-BY-4.0"
                || entry.license_url.as_deref()
                    != Some("https://creativecommons.org/licenses/by/4.0/legalcode")
                || entry.modification_note.trim().is_empty())
        {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Provenance,
                "srd_provenance_incomplete",
                entry.path.clone(),
                "SRD-derived material requires CC BY 4.0 and a modification note",
            );
        }
    }
    for file in &manifest.files {
        let Some(entry) = entries.get(file.provenance_key.as_str()) else {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Provenance,
                "file_provenance_missing",
                Some(file.path.clone()),
                "indexed file has no matching provenance key",
            );
            continue;
        };
        if entry.path.as_deref() != Some(file.path.as_str())
            || entry.digest.as_ref() != Some(&file.digest)
        {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Provenance,
                "file_provenance_mismatch",
                Some(file.path.clone()),
                "file provenance path/digest does not match the immutable index",
            );
        }
    }
    for content in parsed.entries.values() {
        let Some(entry) = entries.get(content.provenance_key.as_str()) else {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Provenance,
                "content_provenance_missing",
                parsed.entry_paths.get(&content.id).cloned(),
                format!("{} has no provenance record", content.id),
            );
            continue;
        };
        if !entry.subject_ids.contains(&content.id)
            || entry.license_id != content.license_class.expected_license_id()
            || (content.availability == ContentAvailability::Active
                && entry.review_status != ReviewStatus::Approved)
        {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Provenance,
                "content_provenance_mismatch",
                parsed.entry_paths.get(&content.id).cloned(),
                format!(
                    "{} provenance does not cover its license/status",
                    content.id
                ),
            );
        }
    }
}

fn validate_safety(inventory: &Inventory, limits: ValidationLimits, report: &mut ValidationReport) {
    const FORBIDDEN: &[&str] = &[
        "<!doctype",
        "<script",
        "<iframe",
        "<object",
        "<embed",
        "javascript:",
        "{{",
        "{%",
        "<?php",
        "ignore previous instructions",
        "ignore all previous",
        "system prompt",
        "execute command",
        "tool call",
        "\"external_fetch\"",
        "\"network_request\"",
        "\"remote_resource\"",
    ];
    for (display_path, path) in &inventory.files {
        let Some(bytes) = read_bounded(
            path,
            limits.max_file_bytes,
            ValidationStage::Safety,
            display_path,
            report,
        ) else {
            continue;
        };
        if bytes.contains(&0) {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Safety,
                "binary_content_forbidden",
                Some(display_path.clone()),
                "pack payloads must be bounded text data",
            );
            continue;
        }
        let Ok(text) = std::str::from_utf8(&bytes) else {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Safety,
                "non_utf8_content",
                Some(display_path.clone()),
                "pack payloads must be UTF-8",
            );
            continue;
        };
        let lower = text.to_ascii_lowercase();
        if let Some(marker) = FORBIDDEN.iter().find(|marker| lower.contains(**marker)) {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Safety,
                "forbidden_instruction_or_markup",
                Some(display_path.clone()),
                format!("payload contains forbidden data marker {marker:?}"),
            );
        }
        if contains_html_tag(&lower) {
            report.push(
                FindingSeverity::Error,
                ValidationStage::Safety,
                "html_markup_forbidden",
                Some(display_path.clone()),
                "arbitrary HTML is not an accepted content format",
            );
        }
    }
}

fn is_valid_semver(value: &str) -> bool {
    if value.is_empty() || value.len() > 64 || !value.is_ascii() {
        return false;
    }
    let mut build_split = value.split('+');
    let base = build_split.next().unwrap_or_default();
    let build = build_split.next();
    if build_split.next().is_some() || build.is_some_and(|part| !valid_semver_labels(part, false)) {
        return false;
    }
    let (core, prerelease) = base
        .split_once('-')
        .map_or((base, None), |(core, prerelease)| (core, Some(prerelease)));
    if prerelease.is_some_and(|part| !valid_semver_labels(part, true)) {
        return false;
    }
    let parts = core.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
                && part.parse::<u64>().is_ok()
        })
}

fn valid_semver_labels(value: &str, numeric_no_leading_zero: bool) -> bool {
    !value.is_empty()
        && value.split('.').all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && (!numeric_no_leading_zero
                    || !label.bytes().all(|byte| byte.is_ascii_digit())
                    || label.len() == 1
                    || !label.starts_with('0'))
        })
}

fn is_valid_pack_id(value: &str) -> bool {
    value.len() <= 128 && value.split('.').count() >= 3 && value.split('.').all(valid_lower_segment)
}

fn is_valid_namespaced_id(value: &str) -> bool {
    value.len() <= 180
        && value.split(':').count() >= 3
        && value.split(':').all(|segment| {
            !segment.is_empty()
                && segment.len() <= 80
                && segment.bytes().all(|byte| {
                    byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_.".contains(&byte)
                })
                && segment
                    .bytes()
                    .next()
                    .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

fn valid_lower_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= 63
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && segment
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && !segment.ends_with('-')
}

fn is_valid_ruleset_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-.".contains(&byte))
}

fn is_valid_capability_id(value: &str) -> bool {
    value.len() <= 100 && value.split('.').count() >= 2 && value.split('.').all(valid_lower_segment)
}

fn is_safe_relative_path(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 240
        || !value.is_ascii()
        || value.contains('\\')
        || value.contains("//")
        || value.ends_with('/')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.'))
    {
        return false;
    }
    let path = Path::new(value);
    !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn is_bounded_text(value: &str, minimum: usize, maximum: usize) -> bool {
    let length = value.chars().count();
    value.trim() == value && (minimum..=maximum).contains(&length)
}

fn is_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if !(bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit()))
    {
        return false;
    }
    let year = value[0..4].parse::<u16>().ok();
    let month = value[5..7].parse::<u8>().ok();
    let day = value[8..10].parse::<u8>().ok();
    year.is_some_and(|year| year >= 2000)
        && month.is_some_and(|month| (1..=12).contains(&month))
        && day.is_some_and(|day| (1..=31).contains(&day))
}

fn valid_hex_color(value: &str) -> bool {
    value.len() == 7
        && value.starts_with('#')
        && value[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn contains_html_tag(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.iter().enumerate().any(|(index, byte)| {
        *byte == b'<'
            && bytes
                .get(index + 1)
                .is_some_and(|next| next.is_ascii_alphabetic() || matches!(*next, b'/' | b'!'))
            && bytes[index + 1..]
                .iter()
                .take(128)
                .any(|next| *next == b'>')
    })
}

fn validate_nonempty_unique<T: Ord + fmt::Debug>(
    values: &[T],
    name: &'static str,
    report: &mut ValidationReport,
) {
    if values.is_empty() || values.iter().collect::<BTreeSet<_>>().len() != values.len() {
        manifest_error(
            report,
            "invalid_manifest_collection",
            match name {
                "categories" => "categories must be non-empty and unique",
                _ => "manifest collection must be non-empty and unique",
            },
        );
    }
}

fn validate_tokens(
    values: &[String],
    validator: fn(&str) -> bool,
    _name: &'static str,
    report: &mut ValidationReport,
) {
    if values.is_empty() || has_duplicates(values) || values.iter().any(|value| !validator(value)) {
        manifest_error(
            report,
            "invalid_manifest_tokens",
            "manifest rulesets/capabilities must be non-empty, valid, and unique",
        );
    }
}

fn has_duplicates<T: Ord>(values: &[T]) -> bool {
    values.iter().collect::<BTreeSet<_>>().len() != values.len()
}

fn relative_display(root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(root)
        .ok()?
        .to_str()
        .map(|value| value.replace(std::path::MAIN_SEPARATOR, "/"))
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    const CAPABILITY: &str = "test.resolve";
    const CONTENT_ID: &str = "test.content:rule:one";
    const PROVENANCE_KEY: &str = "prov:test:subjects";
    const PACK_ID: &str = "dev.example.test-pack";

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    #[derive(Clone)]
    struct TestSpec {
        entries: Vec<ContentEntry>,
        dependencies: Vec<PackDependency>,
        include_subject_provenance: bool,
    }

    impl Default for TestSpec {
        fn default() -> Self {
            Self {
                entries: vec![content_entry(CONTENT_ID, Vec::new())],
                dependencies: Vec::new(),
                include_subject_provenance: true,
            }
        }
    }

    struct TestPack {
        root: PathBuf,
    }

    impl Drop for TestPack {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn versions_and_digests_require_canonical_syntax() {
        for valid in [
            "0.0.0",
            "1.2.3",
            "1.2.3-alpha.1",
            "1.2.3-alpha-1",
            "1.2.3+build-7",
        ] {
            assert_eq!(PackVersion::parse(valid).unwrap().as_str(), valid);
        }
        for invalid in ["1", "1.2", "01.2.3", "1.2.3-01", "1.2.3+", "v1.2.3"] {
            assert!(PackVersion::parse(invalid).is_err(), "accepted {invalid}");
        }
        assert!(Sha256Digest::parse(format!("sha256:{}", "a".repeat(64))).is_ok());
        assert!(Sha256Digest::parse(format!("sha256:{}", "A".repeat(64))).is_err());
    }

    #[test]
    fn valid_data_only_pack_is_accepted() {
        let pack = build_test_pack(TestSpec::default());
        let report = validate_pack(&pack.root, &context());
        assert!(report.activation_allowed(), "{:#?}", report.findings);
        assert_eq!(report.pack.unwrap().id, PACK_ID);
    }

    #[test]
    fn unknown_manifest_fields_are_rejected() {
        let pack = build_test_pack(TestSpec::default());
        let path = pack.root.join(PACK_MANIFEST_FILE);
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["execute_me"] = serde_json::json!(true);
        fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        let report = validate_pack(&pack.root, &context());
        assert!(report.has_code("manifest_schema_invalid"));
        assert!(!report.activation_allowed());
    }

    #[test]
    fn traversal_and_unindexed_files_are_quarantined() {
        let pack = build_test_pack(TestSpec::default());
        rewrite_manifest(&pack.root, |manifest| {
            manifest.files[0].path = "../escape.json".to_owned();
        });
        fs::write(pack.root.join("definitions/extra.json"), b"{}").unwrap();
        let report = validate_pack(&pack.root, &context());
        assert!(report.has_code("invalid_manifest_file"));
        assert!(report.has_code("unindexed_file"));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_never_followed() {
        use std::os::unix::fs::symlink;

        let pack = build_test_pack(TestSpec::default());
        symlink(
            pack.root.join("definitions/content.json"),
            pack.root.join("definitions/link.json"),
        )
        .unwrap();
        let report = validate_pack(&pack.root, &context());
        assert!(report.has_code("symlink_forbidden"));
    }

    #[test]
    fn file_count_and_size_limits_fail_closed() {
        let pack = build_test_pack(TestSpec::default());
        let mut context = context();
        context.limits.max_files = 2;
        context.limits.max_total_bytes = 16;
        let report = validate_pack(&pack.root, &context);
        assert!(report.has_code("file_count_exceeded"));
        assert!(report.has_code("pack_size_exceeded"));
    }

    #[test]
    fn payload_tampering_breaks_digest_and_provenance_pins() {
        let pack = build_test_pack(TestSpec::default());
        let path = pack.root.join("definitions/content.json");
        let mut bytes = fs::read(&path).unwrap();
        bytes.extend_from_slice(b" ");
        fs::write(path, bytes).unwrap();
        let report = validate_pack(&pack.root, &context());
        assert!(report.has_code("payload_digest_mismatch"));
    }

    #[test]
    fn missing_and_cyclic_dependencies_are_quarantined() {
        let dependency = PackDependency {
            id: "dev.example.dependency".to_owned(),
            version: PackVersion::parse("1.0.0").unwrap(),
            digest: Sha256Digest::of_bytes(b"dependency"),
        };
        let pack = build_test_pack(TestSpec {
            dependencies: vec![dependency.clone()],
            ..TestSpec::default()
        });
        let missing = validate_pack(&pack.root, &context());
        assert!(missing.has_code("dependency_missing"));

        let mut cyclic_context = context();
        cyclic_context.installed_packs.insert(
            dependency.id.clone(),
            InstalledPack {
                id: dependency.id,
                version: dependency.version,
                digest: dependency.digest,
                dependencies: BTreeSet::from([PACK_ID.to_owned()]),
                content_ids: BTreeSet::new(),
            },
        );
        let cyclic = validate_pack(&pack.root, &cyclic_context);
        assert!(cyclic.has_code("dependency_cycle"));
    }

    #[test]
    fn unsupported_active_capabilities_are_quarantined() {
        let pack = build_test_pack(TestSpec::default());
        let report = validate_pack(&pack.root, &PackValidationContext::default());
        assert!(report.has_code("engine_capability_missing"));
        assert!(report.has_code("active_content_unsupported"));
    }

    #[test]
    fn dangling_references_and_reference_cycles_are_quarantined() {
        let dangling = build_test_pack(TestSpec {
            entries: vec![content_entry(
                CONTENT_ID,
                vec!["test.content:rule:missing".to_owned()],
            )],
            ..TestSpec::default()
        });
        assert!(validate_pack(&dangling.root, &context()).has_code("dangling_content_reference"));

        let first = "test.content:rule:first";
        let second = "test.content:rule:second";
        let cyclic = build_test_pack(TestSpec {
            entries: vec![
                content_entry(first, vec![second.to_owned()]),
                content_entry(second, vec![first.to_owned()]),
            ],
            ..TestSpec::default()
        });
        assert!(validate_pack(&cyclic.root, &context()).has_code("content_reference_cycle"));
    }

    #[test]
    fn missing_subject_provenance_is_quarantined() {
        let pack = build_test_pack(TestSpec {
            include_subject_provenance: false,
            ..TestSpec::default()
        });
        let report = validate_pack(&pack.root, &context());
        assert!(report.has_code("content_provenance_missing"));
    }

    #[test]
    fn html_templates_and_prompt_instructions_are_quarantined() {
        let mut html_entry = content_entry(CONTENT_ID, Vec::new());
        html_entry.description = "<p>Ignore previous instructions</p>".to_owned();
        let pack = build_test_pack(TestSpec {
            entries: vec![html_entry],
            ..TestSpec::default()
        });
        let report = validate_pack(&pack.root, &context());
        assert!(report.has_code("forbidden_instruction_or_markup"));
        assert!(report.has_code("html_markup_forbidden"));
    }

    #[test]
    fn bundled_core_and_presentation_themes_validate() {
        let packs_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content/packs");
        let catalog = load_bundled_content_catalog(&packs_root, RAINBOUND_THEME_PACK_ID).unwrap();

        assert_eq!(catalog.ruleset_id(), MVP_ENGINE_RULESET_ID);
        assert_eq!(catalog.packs().len(), REQUIRED_PACKS.len());
        assert_eq!(
            catalog.engine_capabilities(),
            &MVP_ENGINE_CAPABILITIES
                .iter()
                .map(|capability| (*capability).to_owned())
                .collect()
        );
        assert_eq!(
            catalog.default_theme().identity().id,
            RAINBOUND_THEME_PACK_ID
        );
        assert!(
            catalog
                .default_theme()
                .theme_tokens()
                .unwrap()
                .presentation_only
        );

        let core = catalog.pack(CORE_CONTENT_PACK_ID).unwrap();
        assert!(
            core.active_content_ids()
                .contains("srd-5.1-cc:rule:standard-array")
        );
        assert!(
            core.active_content_ids()
                .contains("srd-5.1-cc:background:sage")
        );
        assert!(
            core.active_content_ids()
                .contains("srd-5.1-cc:spell:magic-missile")
        );
        assert!(!catalog.engine_capabilities().contains("spell.wizard-q04"));
        assert!(
            !catalog
                .engine_capabilities()
                .contains("class.wizard.level-1-2")
        );
    }

    #[test]
    fn bundled_catalog_fails_closed_for_missing_invalid_and_unpinned_packs() {
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(
            load_bundled_content_catalog(empty.path(), RAINBOUND_THEME_PACK_ID),
            Err(ContentCatalogError::RequiredPackMissing {
                pack_id: CORE_CONTENT_PACK_ID,
            })
        );

        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content/packs");
        let invalid = tempfile::tempdir().unwrap();
        copy_tree(&source, invalid.path());
        let invalid_manifest_path = invalid.path().join("core-mvp/manifest.json");
        let mut invalid_manifest: PackManifest =
            serde_json::from_slice(&fs::read(&invalid_manifest_path).unwrap()).unwrap();
        invalid_manifest.display_name.push_str(" tampered");
        fs::write(
            &invalid_manifest_path,
            serde_json::to_vec_pretty(&invalid_manifest).unwrap(),
        )
        .unwrap();
        let error = load_bundled_content_catalog(invalid.path(), RAINBOUND_THEME_PACK_ID)
            .expect_err("a stale self-digest must quarantine the pack");
        assert!(matches!(
            error,
            ContentCatalogError::RequiredPackInvalid {
                pack_id: CORE_CONTENT_PACK_ID,
                ..
            }
        ));

        let unpinned = tempfile::tempdir().unwrap();
        copy_tree(&source, unpinned.path());
        let unpinned_manifest_path = unpinned.path().join("core-mvp/manifest.json");
        let mut unpinned_manifest: PackManifest =
            serde_json::from_slice(&fs::read(&unpinned_manifest_path).unwrap()).unwrap();
        unpinned_manifest.display_name.push_str(" reviewed variant");
        unpinned_manifest.digest = compute_manifest_digest(&unpinned_manifest);
        fs::write(
            &unpinned_manifest_path,
            serde_json::to_vec_pretty(&unpinned_manifest).unwrap(),
        )
        .unwrap();
        assert_eq!(
            load_bundled_content_catalog(unpinned.path(), RAINBOUND_THEME_PACK_ID),
            Err(ContentCatalogError::ExactPinMismatch {
                pack_id: CORE_CONTENT_PACK_ID,
            })
        );
    }

    fn context() -> PackValidationContext {
        PackValidationContext {
            engine_rulesets: BTreeSet::from(["srd-5.1-cc".to_owned()]),
            engine_capabilities: BTreeSet::from([CAPABILITY.to_owned()]),
            ..PackValidationContext::default()
        }
    }

    fn content_entry(id: &str, references: Vec<String>) -> ContentEntry {
        let effects = if references.is_empty() {
            vec![TypedEffect::EnginePolicy {
                capability: CAPABILITY.to_owned(),
            }]
        } else {
            vec![TypedEffect::Composes {
                content_ids: references.clone(),
            }]
        };
        ContentEntry {
            schema_version: 1,
            id: id.to_owned(),
            kind: ContentKind::Rule,
            availability: ContentAvailability::Active,
            display_name: "Test rule".to_owned(),
            description: "Original bounded test content.".to_owned(),
            ruleset_id: "srd-5.1-cc".to_owned(),
            required_engine_capabilities: vec![CAPABILITY.to_owned()],
            references,
            source_key: "source:test:rule-one".to_owned(),
            license_class: LicenseClass::OriginalPrivateEvaluation,
            provenance_key: PROVENANCE_KEY.to_owned(),
            effects,
        }
    }

    fn build_test_pack(spec: TestSpec) -> TestPack {
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "manchester-content-test-{}-{timestamp}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("definitions")).unwrap();
        fs::create_dir_all(root.join("mechanics")).unwrap();
        fs::create_dir_all(root.join("notices")).unwrap();

        let document = ContentDocument {
            content_schema: CONTENT_DOCUMENT_SCHEMA.to_owned(),
            document_id: "test.document:content:one".to_owned(),
            entries: spec.entries.clone(),
        };
        let content_bytes = serde_json::to_vec_pretty(&document).unwrap();
        fs::write(root.join("definitions/content.json"), &content_bytes).unwrap();

        let traces = spec
            .entries
            .iter()
            .map(|entry| TraceabilityEntry {
                mechanic_id: entry.id.clone(),
                availability: entry.availability,
                source_key: entry.source_key.clone(),
                source_location: "Original test fixture, rule one".to_owned(),
                license_class: entry.license_class,
                provenance_key: entry.provenance_key.clone(),
                implementation_symbols: vec!["test::resolve".to_owned()],
                test_ids: vec!["test:content:valid-pack".to_owned()],
                consuming_content: vec![entry.id.clone()],
                required_engine_capabilities: entry.required_engine_capabilities.clone(),
                modification_note: "Original fixture; no source prose copied.".to_owned(),
            })
            .collect();
        let traceability = MechanicsTraceability {
            traceability_schema: TRACEABILITY_SCHEMA.to_owned(),
            pack_id: PACK_ID.to_owned(),
            pack_version: PackVersion::parse("1.0.0").unwrap(),
            entries: traces,
        };
        let trace_bytes = serde_json::to_vec_pretty(&traceability).unwrap();
        fs::write(root.join("mechanics/traceability.json"), &trace_bytes).unwrap();
        let notice_bytes = b"Private test fixture. No redistribution license is granted.\n";
        fs::write(root.join("notices/NOTICE.txt"), notice_bytes).unwrap();

        let content_digest = Sha256Digest::of_bytes(&content_bytes);
        let trace_digest = Sha256Digest::of_bytes(&trace_bytes);
        let notice_digest = Sha256Digest::of_bytes(notice_bytes);
        let file_entries = vec![
            provenance_file(
                "prov:test:file-content",
                "definitions/content.json",
                content_digest.clone(),
            ),
            provenance_file(
                "prov:test:file-trace",
                "mechanics/traceability.json",
                trace_digest.clone(),
            ),
            provenance_file(
                "prov:test:file-notice",
                "notices/NOTICE.txt",
                notice_digest.clone(),
            ),
        ];
        let mut provenance_entries = file_entries;
        if spec.include_subject_provenance {
            provenance_entries.push(ProvenanceEntry {
                provenance_key: PROVENANCE_KEY.to_owned(),
                path: None,
                digest: None,
                subject_ids: spec.entries.iter().map(|entry| entry.id.clone()).collect(),
                origin: ProvenanceOrigin::Original,
                title: "Original test subjects".to_owned(),
                creator: "Test suite".to_owned(),
                rightsholder: "Test suite".to_owned(),
                source_locator: "original:test-fixture".to_owned(),
                license_id: "LicenseRef-Manchester-Arcana-Private-Evaluation".to_owned(),
                license_url: None,
                required_notice: "Private evaluation only.".to_owned(),
                modification_note: "Created for validator tests.".to_owned(),
                created_or_retrieved_at: "2026-07-14".to_owned(),
                reviewer: "test-suite".to_owned(),
                review_status: ReviewStatus::Approved,
                ruleset_ids: vec!["srd-5.1-cc".to_owned()],
                pack_id: PACK_ID.to_owned(),
                pack_version: PackVersion::parse("1.0.0").unwrap(),
            });
        }
        let provenance = ProvenanceManifest {
            provenance_schema: PROVENANCE_SCHEMA.to_owned(),
            pack_id: PACK_ID.to_owned(),
            pack_version: PackVersion::parse("1.0.0").unwrap(),
            entries: provenance_entries,
        };
        let provenance_bytes = serde_json::to_vec_pretty(&provenance).unwrap();
        fs::write(root.join("provenance.json"), &provenance_bytes).unwrap();

        let placeholder = Sha256Digest::of_bytes(b"placeholder");
        let mut manifest = PackManifest {
            pack_schema: CONTENT_PACK_SCHEMA.to_owned(),
            id: PACK_ID.to_owned(),
            version: PackVersion::parse("1.0.0").unwrap(),
            digest: placeholder,
            display_name: "Validator test pack".to_owned(),
            categories: vec![PackCategory::RulesCompendium],
            compatible_rulesets: vec!["srd-5.1-cc".to_owned()],
            required_engine_capabilities: vec![CAPABILITY.to_owned()],
            dependencies: spec.dependencies,
            license: PackLicense {
                license_id: "LicenseRef-Manchester-Arcana-Private-Evaluation".to_owned(),
                license_url: None,
                notice_path: "notices/NOTICE.txt".to_owned(),
                allowed_content_license_ids: vec![
                    "LicenseRef-Manchester-Arcana-Private-Evaluation".to_owned(),
                ],
            },
            provenance_manifest: "provenance.json".to_owned(),
            provenance_digest: Sha256Digest::of_bytes(&provenance_bytes),
            content_roots: vec![
                "definitions".to_owned(),
                "mechanics".to_owned(),
                "notices".to_owned(),
            ],
            files: vec![
                ManifestFile {
                    path: "definitions/content.json".to_owned(),
                    digest: content_digest,
                    kind: ManifestFileKind::Content,
                    provenance_key: "prov:test:file-content".to_owned(),
                },
                ManifestFile {
                    path: "mechanics/traceability.json".to_owned(),
                    digest: trace_digest,
                    kind: ManifestFileKind::MechanicsTraceability,
                    provenance_key: "prov:test:file-trace".to_owned(),
                },
                ManifestFile {
                    path: "notices/NOTICE.txt".to_owned(),
                    digest: notice_digest,
                    kind: ManifestFileKind::Notice,
                    provenance_key: "prov:test:file-notice".to_owned(),
                },
            ],
        };
        manifest.digest = compute_manifest_digest(&manifest);
        fs::write(
            root.join(PACK_MANIFEST_FILE),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        TestPack { root }
    }

    fn provenance_file(key: &str, path: &str, digest: Sha256Digest) -> ProvenanceEntry {
        ProvenanceEntry {
            provenance_key: key.to_owned(),
            path: Some(path.to_owned()),
            digest: Some(digest),
            subject_ids: Vec::new(),
            origin: ProvenanceOrigin::Original,
            title: format!("Test file {path}"),
            creator: "Test suite".to_owned(),
            rightsholder: "Test suite".to_owned(),
            source_locator: "original:test-fixture".to_owned(),
            license_id: "LicenseRef-Manchester-Arcana-Private-Evaluation".to_owned(),
            license_url: None,
            required_notice: "Private evaluation only.".to_owned(),
            modification_note: "Created for validator tests.".to_owned(),
            created_or_retrieved_at: "2026-07-14".to_owned(),
            reviewer: "test-suite".to_owned(),
            review_status: ReviewStatus::Approved,
            ruleset_ids: vec!["srd-5.1-cc".to_owned()],
            pack_id: PACK_ID.to_owned(),
            pack_version: PackVersion::parse("1.0.0").unwrap(),
        }
    }

    fn rewrite_manifest(root: &Path, mutate: impl FnOnce(&mut PackManifest)) {
        let path = root.join(PACK_MANIFEST_FILE);
        let mut manifest: PackManifest = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        mutate(&mut manifest);
        manifest.digest = compute_manifest_digest(&manifest);
        fs::write(path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    }

    fn copy_tree(source: &Path, destination: &Path) {
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let destination = destination.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                fs::create_dir(&destination).unwrap();
                copy_tree(&entry.path(), &destination);
            } else {
                fs::copy(entry.path(), destination).unwrap();
            }
        }
    }
}
