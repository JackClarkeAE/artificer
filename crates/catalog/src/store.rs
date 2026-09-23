use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use serde::{Deserialize, Serialize};

use crate::{
    CatalogError, ContentDigest, MAX_INDEX_ENTRIES, MAX_PACKAGE_BYTES, PartDefinitionId, PartKind,
    PartMetadata, PartPackage, PartRevision, invalid, validate_text,
};

const OBJECTS_DIRECTORY: &str = "objects";
const PREVIEWS_DIRECTORY: &str = "previews";
const PREVIEW_IMAGE_SUFFIX: &str = ".png";
const PREVIEW_FACTS_SUFFIX: &str = ".json";
/// The largest preview image the store keeps or hands back.
pub const MAX_PREVIEW_IMAGE_BYTES: usize = 256 * 1024;
const MAX_PREVIEW_FACTS_BYTES: usize = 4 * 1024;
const MAX_PREVIEW_TEXT_BYTES: usize = 128;
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
const REFERENCES_DIRECTORY: &str = "refs";
const OBJECT_SUFFIX: &str = ".part.json";
const REFERENCE_SUFFIX: &str = ".ref";
const MAX_REFERENCE_BYTES: usize = 128;
const MAX_SEARCH_TEXT_BYTES: usize = 1_024;
const MAX_SEARCH_RESULTS: usize = 10_000;

static TEMPORARY_FILE_COUNTER: AtomicU64 = AtomicU64::new(1);

/// What a published part looks like and roughly measures, kept beside its
/// package so the library can show it without evaluating the part.
///
/// A preview is derived rather than authored. It is not part of the package's
/// content address, and a later build may draw it again; the package it
/// describes stays immutable either way.
#[derive(Clone, Debug, PartialEq)]
pub struct PartPreview {
    /// A small PNG of the part in an isometric view fitted to it.
    pub image_png: Vec<u8>,
    pub facts: PartPreviewFacts,
}

/// The measurements a preview was drawn with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartPreviewFacts {
    /// The drawn part's bounding box along X, Y and Z, in millimetres.
    pub extents_mm: [f64; 3],
    /// For each axis, the parameter that sets that extent, if one does; the
    /// extent is then only the drawn sample's.
    pub driven_by: [Option<String>; 3],
    /// The parameter values the part was drawn with, as a person reads them
    /// ("Length 100 mm"), when it has parameters.
    pub sample: Option<String>,
}

impl PartPreviewFacts {
    fn validate(&self) -> Result<(), CatalogError> {
        if self
            .extents_mm
            .iter()
            .any(|extent| !extent.is_finite() || *extent < 0.0)
        {
            return Err(invalid(
                "preview extents",
                "must be finite and not negative",
            ));
        }
        for name in self.driven_by.iter().flatten() {
            validate_text(
                name,
                "preview parameter name",
                MAX_PREVIEW_TEXT_BYTES,
                false,
            )?;
        }
        if let Some(sample) = &self.sample {
            validate_text(sample, "preview sample", MAX_PREVIEW_TEXT_BYTES, false)?;
        }
        Ok(())
    }
}

impl PartPreview {
    fn validate(&self) -> Result<(), CatalogError> {
        if self.image_png.len() > MAX_PREVIEW_IMAGE_BYTES {
            return Err(CatalogError::ResourceLimit {
                resource: "preview image",
                limit: MAX_PREVIEW_IMAGE_BYTES,
                actual: self.image_png.len(),
            });
        }
        if !self.image_png.starts_with(&PNG_SIGNATURE) {
            return Err(invalid("preview image", "is not a PNG"));
        }
        self.facts.validate()
    }
}

/// Compact immutable entry used by the library browser and search index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogEntry {
    definition_id: PartDefinitionId,
    revision: PartRevision,
    digest: ContentDigest,
    kind: PartKind,
    metadata: PartMetadata,
    parameter_count: usize,
    required_parameter_count: usize,
}

impl CatalogEntry {
    fn from_package(package: &PartPackage) -> Self {
        let definition = package.definition();
        Self {
            definition_id: definition.id().clone(),
            revision: definition.revision(),
            digest: package.content_digest(),
            kind: definition.kind(),
            metadata: definition.metadata().clone(),
            parameter_count: definition.parameters().len(),
            required_parameter_count: definition
                .parameters()
                .iter()
                .filter(|parameter| parameter.requires_input())
                .count(),
        }
    }

    #[must_use]
    pub const fn definition_id(&self) -> &PartDefinitionId {
        &self.definition_id
    }

    #[must_use]
    pub const fn revision(&self) -> PartRevision {
        self.revision
    }

    #[must_use]
    pub const fn digest(&self) -> ContentDigest {
        self.digest
    }

    #[must_use]
    pub const fn kind(&self) -> PartKind {
        self.kind
    }

    #[must_use]
    pub const fn metadata(&self) -> &PartMetadata {
        &self.metadata
    }

    #[must_use]
    pub const fn parameter_count(&self) -> usize {
        self.parameter_count
    }

    #[must_use]
    pub const fn required_parameter_count(&self) -> usize {
        self.required_parameter_count
    }

    fn matches(&self, query: &SearchQuery) -> bool {
        if query.kind.is_some_and(|kind| kind != self.kind) {
            return false;
        }
        if query.category.as_ref().is_some_and(|expected| {
            self.metadata
                .category()
                .is_none_or(|actual| !actual.eq_ignore_ascii_case(expected))
        }) {
            return false;
        }
        if !query.required_tags.iter().all(|expected| {
            self.metadata
                .tags()
                .iter()
                .any(|actual| actual.eq_ignore_ascii_case(expected))
        }) {
            return false;
        }
        if query.text.trim().is_empty() {
            return true;
        }

        let mut searchable = String::new();
        for value in [
            Some(self.definition_id.as_str()),
            Some(self.metadata.name()),
            self.metadata.description(),
            self.metadata.category(),
            self.metadata.material(),
            self.metadata.part_number(),
        ]
        .into_iter()
        .flatten()
        {
            searchable.push_str(value);
            searchable.push(' ');
        }
        for tag in self.metadata.tags() {
            searchable.push_str(tag);
            searchable.push(' ');
        }
        let searchable = searchable.to_lowercase();
        query
            .text
            .split_whitespace()
            .map(str::to_lowercase)
            .all(|term| searchable.contains(&term))
    }
}

/// Deterministic, in-memory catalog index rebuilt entirely from internal refs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CatalogIndex {
    entries: Vec<CatalogEntry>,
}

impl CatalogIndex {
    fn from_entries(mut entries: Vec<CatalogEntry>) -> Self {
        sort_entries(&mut entries);
        Self { entries }
    }

    #[must_use]
    pub fn entries(&self) -> &[CatalogEntry] {
        &self.entries
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn search(&self, query: &SearchQuery) -> Vec<CatalogEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.matches(query))
            .take(query.limit)
            .cloned()
            .collect()
    }
}

fn sort_entries(entries: &mut [CatalogEntry]) {
    entries.sort_by(|left, right| {
        left.metadata
            .name()
            .to_lowercase()
            .cmp(&right.metadata.name().to_lowercase())
            .then_with(|| left.definition_id.cmp(&right.definition_id))
            // Show the newest authored revision first within one definition.
            .then_with(|| right.revision.cmp(&left.revision))
            .then_with(|| left.digest.cmp(&right.digest))
    });
}

/// Validated filters for deterministic local search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchQuery {
    text: String,
    kind: Option<PartKind>,
    category: Option<String>,
    required_tags: BTreeSet<String>,
    limit: usize,
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            text: String::new(),
            kind: None,
            category: None,
            required_tags: BTreeSet::new(),
            limit: 250,
        }
    }
}

impl SearchQuery {
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    #[must_use]
    pub const fn with_kind(mut self, kind: PartKind) -> Self {
        self.kind = Some(kind);
        self
    }

    #[must_use]
    pub fn with_category(mut self, category: impl Into<String>) -> Self {
        self.category = Some(category.into());
        self
    }

    #[must_use]
    pub fn with_required_tag(mut self, tag: impl Into<String>) -> Self {
        self.required_tags.insert(tag.into());
        self
    }

    #[must_use]
    pub const fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    fn validate(&self) -> Result<(), CatalogError> {
        if self.text.len() > MAX_SEARCH_TEXT_BYTES {
            return Err(CatalogError::ResourceLimit {
                resource: "catalog search text",
                limit: MAX_SEARCH_TEXT_BYTES,
                actual: self.text.len(),
            });
        }
        if self.limit > MAX_SEARCH_RESULTS {
            return Err(CatalogError::ResourceLimit {
                resource: "catalog search results",
                limit: MAX_SEARCH_RESULTS,
                actual: self.limit,
            });
        }
        if self
            .category
            .as_ref()
            .is_some_and(|category| category.trim().is_empty())
            || self.required_tags.iter().any(|tag| tag.trim().is_empty())
        {
            return Err(CatalogError::InvalidField {
                field: "catalog search filter",
                reason: "category and tag filters must not be empty".into(),
            });
        }
        Ok(())
    }
}

/// Non-authoritative relative-path diagnostic from a tolerant index rebuild.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RejectedCatalogEntry {
    relative_path: String,
    message: String,
}

impl RejectedCatalogEntry {
    #[must_use]
    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Outcome of rebuilding the disposable index from immutable package refs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexRebuildReport {
    accepted: usize,
    rejected: Vec<RejectedCatalogEntry>,
}

impl IndexRebuildReport {
    #[must_use]
    pub const fn accepted(&self) -> usize {
        self.accepted
    }

    #[must_use]
    pub fn rejected(&self) -> &[RejectedCatalogEntry] {
        &self.rejected
    }
}

/// Local content-addressed package store with an entirely rebuildable index.
///
/// Objects live under `objects/`, while a small internal `refs/` record pins one
/// digest to each `(definition ID, revision)`. Both are created without
/// overwriting existing paths. A crash can at worst leave an unreachable object
/// or temporary file; it cannot partially replace a published revision.
#[derive(Debug)]
pub struct CatalogStore {
    root: PathBuf,
    index: RwLock<CatalogIndex>,
    rejected: RwLock<Vec<RejectedCatalogEntry>>,
}

impl CatalogStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, CatalogError> {
        let root = root.into();
        ensure_directory(&root)?;
        ensure_directory(&root.join(OBJECTS_DIRECTORY))?;
        ensure_directory(&root.join(REFERENCES_DIRECTORY))?;
        let store = Self {
            root,
            index: RwLock::new(CatalogIndex::default()),
            rejected: RwLock::new(Vec::new()),
        };
        store.rebuild_index()?;
        Ok(store)
    }

    /// Publishes the exact revision atomically and idempotently.
    ///
    /// Reusing an existing definition ID and revision with different content is
    /// rejected. Authors must create a new revision instead.
    pub fn publish(&self, package: &PartPackage) -> Result<ContentDigest, CatalogError> {
        package.verify()?;
        let bytes = package.to_json_bytes()?;
        let digest = package.content_digest();
        // A revision that already names other content is refused before any
        // object is written. Checking only after the object was in place left
        // an unreachable copy behind on every refused publish.
        let definition = package.definition();
        let reference_path = self.reference_path(definition.id(), definition.revision());
        if reference_path.exists() {
            let existing = parse_reference_bytes(&read_limited_regular_file(
                &reference_path,
                MAX_REFERENCE_BYTES,
            )?)?;
            if existing != digest {
                return Err(CatalogError::RevisionConflict {
                    definition: definition.id().clone(),
                    revision: definition.revision(),
                    existing,
                    attempted: digest,
                });
            }
        }
        let object_path = self.object_path(digest);
        let object_parent = object_path
            .parent()
            .ok_or_else(|| CatalogError::UnsafeFilesystemEntry("object has no parent".into()))?;
        ensure_directory(object_parent)?;
        atomic_create_or_verify(&object_path, &bytes, MAX_PACKAGE_BYTES)?;

        let reference_directory = self.reference_directory(definition.id());
        ensure_directory(&reference_directory)?;
        let reference_bytes = format!("{digest}\n").into_bytes();
        match atomic_create_or_compare(&reference_path, &reference_bytes, MAX_REFERENCE_BYTES)? {
            CreateOutcome::Created | CreateOutcome::Identical => {}
            CreateOutcome::Different(existing_bytes) => {
                let existing = parse_reference_bytes(&existing_bytes)?;
                return Err(CatalogError::RevisionConflict {
                    definition: definition.id().clone(),
                    revision: definition.revision(),
                    existing,
                    attempted: digest,
                });
            }
        }

        let entry = CatalogEntry::from_package(package);
        let mut index = self.write_index()?;
        index.entries.retain(|existing| {
            existing.definition_id != entry.definition_id || existing.revision != entry.revision
        });
        index.entries.push(entry);
        sort_entries(&mut index.entries);
        Ok(digest)
    }

    /// Loads and verifies one immutable object by its digest.
    pub fn load(&self, digest: ContentDigest) -> Result<PartPackage, CatalogError> {
        let path = self.object_path(digest);
        if !path.exists() {
            return Err(CatalogError::ObjectNotFound(digest));
        }
        let bytes = read_limited_regular_file(&path, MAX_PACKAGE_BYTES)?;
        let package = PartPackage::from_json_bytes(&bytes)?;
        if package.content_digest() != digest {
            return Err(CatalogError::DigestMismatch {
                expected: digest,
                actual: package.content_digest(),
            });
        }
        Ok(package)
    }

    /// Resolves an exact immutable authored revision and verifies both layers.
    pub fn resolve(
        &self,
        definition: &PartDefinitionId,
        revision: PartRevision,
    ) -> Result<PartPackage, CatalogError> {
        let path = self.reference_path(definition, revision);
        if !path.exists() {
            return Err(CatalogError::RevisionNotFound {
                definition: definition.clone(),
                revision,
            });
        }
        let bytes = read_limited_regular_file(&path, MAX_REFERENCE_BYTES)?;
        let digest = parse_reference_bytes(&bytes)?;
        let package = self.load(digest)?;
        if package.definition().id() != definition || package.definition().revision() != revision {
            return Err(CatalogError::UnsafeFilesystemEntry(format!(
                "reference {definition}/{revision} points to a package with another identity"
            )));
        }
        Ok(package)
    }

    /// Keeps a preview beside a published package, replacing any earlier one.
    ///
    /// Only a package in the store can have a preview. The image and its
    /// facts are each replaced atomically, the image first, so a reader sees
    /// either the old pair, the new pair, or the new image with the old
    /// facts — never a half-written file.
    pub fn save_preview(
        &self,
        digest: ContentDigest,
        preview: &PartPreview,
    ) -> Result<(), CatalogError> {
        if !self.object_path(digest).exists() {
            return Err(CatalogError::ObjectNotFound(digest));
        }
        preview.validate()?;
        let facts = serde_json::to_vec(&preview.facts)?;
        let (image_path, facts_path) = self.preview_paths(digest);
        atomic_replace(&image_path, &preview.image_png, MAX_PREVIEW_IMAGE_BYTES)?;
        atomic_replace(&facts_path, &facts, MAX_PREVIEW_FACTS_BYTES)?;
        Ok(())
    }

    /// The preview kept for a package, if there is a readable one.
    ///
    /// A missing, oversized or malformed preview is `None` rather than an
    /// error: it is derived data, and the answer is to draw it again.
    pub fn preview(&self, digest: ContentDigest) -> Result<Option<PartPreview>, CatalogError> {
        let (image_path, facts_path) = self.preview_paths(digest);
        if !image_path.exists() || !facts_path.exists() {
            return Ok(None);
        }
        let Ok(image_png) = read_limited_regular_file(&image_path, MAX_PREVIEW_IMAGE_BYTES) else {
            return Ok(None);
        };
        let Ok(facts) = read_limited_regular_file(&facts_path, MAX_PREVIEW_FACTS_BYTES) else {
            return Ok(None);
        };
        let Ok(facts) = serde_json::from_slice::<PartPreviewFacts>(&facts) else {
            return Ok(None);
        };
        let preview = PartPreview { image_png, facts };
        Ok(preview.validate().is_ok().then_some(preview))
    }

    fn preview_paths(&self, digest: ContentDigest) -> (PathBuf, PathBuf) {
        let hex = digest.to_hex();
        let directory = self.root.join(PREVIEWS_DIRECTORY).join(&hex[..2]);
        (
            directory.join(format!("{}{PREVIEW_IMAGE_SUFFIX}", &hex[2..])),
            directory.join(format!("{}{PREVIEW_FACTS_SUFFIX}", &hex[2..])),
        )
    }

    /// Reconstructs the disposable search index and tolerantly reports corrupt
    /// or unexpected individual refs. Root-level I/O and global resource-limit
    /// failures remain fatal.
    pub fn rebuild_index(&self) -> Result<IndexRebuildReport, CatalogError> {
        let references = self.root.join(REFERENCES_DIRECTORY);
        let mut accepted = BTreeMap::<(PartDefinitionId, PartRevision), CatalogEntry>::new();
        let mut rejected = Vec::new();
        let mut discovered = 0_usize;

        for definition_entry in sorted_directory_entries(&references)? {
            let definition_path = definition_entry.path();
            let relative = self.relative_diagnostic(&definition_path);
            let metadata = match fs::symlink_metadata(&definition_path) {
                Ok(metadata) if metadata.file_type().is_dir() => metadata,
                Ok(_) => {
                    rejected.push(rejection(relative, "expected a definition directory"));
                    continue;
                }
                Err(error) => {
                    rejected.push(rejection(relative, error));
                    continue;
                }
            };
            if metadata.file_type().is_symlink() {
                rejected.push(rejection(relative, "symbolic links are not accepted"));
                continue;
            }
            let Some(name) = definition_path.file_name().and_then(|name| name.to_str()) else {
                rejected.push(rejection(
                    relative,
                    "definition directory is not valid UTF-8",
                ));
                continue;
            };
            let definition = match PartDefinitionId::parse(name) {
                Ok(definition) => definition,
                Err(error) => {
                    rejected.push(rejection(relative, error));
                    continue;
                }
            };

            for reference_entry in sorted_directory_entries(&definition_path)? {
                discovered = discovered.saturating_add(1);
                if discovered > MAX_INDEX_ENTRIES {
                    return Err(CatalogError::ResourceLimit {
                        resource: "catalog index entries",
                        limit: MAX_INDEX_ENTRIES,
                        actual: discovered,
                    });
                }
                let reference_path = reference_entry.path();
                let relative = self.relative_diagnostic(&reference_path);
                match self.read_catalog_entry(&definition, &reference_path) {
                    Ok(entry) => {
                        let key = (entry.definition_id.clone(), entry.revision);
                        if accepted.insert(key, entry).is_some() {
                            rejected.push(rejection(relative, "duplicate catalog revision"));
                        }
                    }
                    Err(error) => rejected.push(rejection(relative, error)),
                }
            }
        }

        let index = CatalogIndex::from_entries(accepted.into_values().collect());
        let report = IndexRebuildReport {
            accepted: index.len(),
            rejected: rejected.clone(),
        };
        *self.write_index()? = index;
        *self.write_rejected()? = rejected;
        Ok(report)
    }

    pub fn search(&self, query: &SearchQuery) -> Result<Vec<CatalogEntry>, CatalogError> {
        query.validate()?;
        Ok(self.read_index()?.search(query))
    }

    pub fn index_snapshot(&self) -> Result<CatalogIndex, CatalogError> {
        Ok(self.read_index()?.clone())
    }

    pub fn rejected_snapshot(&self) -> Result<Vec<RejectedCatalogEntry>, CatalogError> {
        Ok(self
            .rejected
            .read()
            .map_err(|_| CatalogError::LockPoisoned)?
            .clone())
    }

    fn read_catalog_entry(
        &self,
        expected_definition: &PartDefinitionId,
        path: &Path,
    ) -> Result<CatalogEntry, CatalogError> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Err(CatalogError::UnsafeFilesystemEntry(
                "reference must be a regular non-symlink file".into(),
            ));
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return Err(CatalogError::UnsafeFilesystemEntry(
                "reference name is not valid UTF-8".into(),
            ));
        };
        let Some(revision_text) = file_name.strip_suffix(REFERENCE_SUFFIX) else {
            return Err(CatalogError::UnsafeFilesystemEntry(
                "reference must use the .ref suffix".into(),
            ));
        };
        let expected_revision = PartRevision::from_str(revision_text)?;
        let bytes = read_limited_regular_file(path, MAX_REFERENCE_BYTES)?;
        let digest = parse_reference_bytes(&bytes)?;
        let package = self.load(digest)?;
        let actual = package.definition();
        if actual.id() != expected_definition || actual.revision() != expected_revision {
            return Err(CatalogError::UnsafeFilesystemEntry(format!(
                "ref identity {expected_definition}/{expected_revision} does not match package {}/{}",
                actual.id(),
                actual.revision()
            )));
        }
        Ok(CatalogEntry::from_package(&package))
    }

    fn object_path(&self, digest: ContentDigest) -> PathBuf {
        let hex = digest.to_hex();
        self.root
            .join(OBJECTS_DIRECTORY)
            .join(&hex[..2])
            .join(format!("{}{OBJECT_SUFFIX}", &hex[2..]))
    }

    fn reference_directory(&self, definition: &PartDefinitionId) -> PathBuf {
        self.root
            .join(REFERENCES_DIRECTORY)
            .join(definition.as_str())
    }

    fn reference_path(&self, definition: &PartDefinitionId, revision: PartRevision) -> PathBuf {
        self.reference_directory(definition)
            .join(format!("{revision}{REFERENCE_SUFFIX}"))
    }

    fn relative_diagnostic(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    }

    fn read_index(&self) -> Result<RwLockReadGuard<'_, CatalogIndex>, CatalogError> {
        self.index.read().map_err(|_| CatalogError::LockPoisoned)
    }

    fn write_index(&self) -> Result<RwLockWriteGuard<'_, CatalogIndex>, CatalogError> {
        self.index.write().map_err(|_| CatalogError::LockPoisoned)
    }

    fn write_rejected(
        &self,
    ) -> Result<RwLockWriteGuard<'_, Vec<RejectedCatalogEntry>>, CatalogError> {
        self.rejected
            .write()
            .map_err(|_| CatalogError::LockPoisoned)
    }
}

fn rejection(relative_path: String, error: impl std::fmt::Display) -> RejectedCatalogEntry {
    RejectedCatalogEntry {
        relative_path,
        message: error.to_string(),
    }
}

fn ensure_directory(path: &Path) -> Result<(), CatalogError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
                return Err(CatalogError::UnsafeFilesystemEntry(format!(
                    "{} must be a regular directory",
                    path.display()
                )));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path)?;
            let metadata = fs::symlink_metadata(path)?;
            if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
                return Err(CatalogError::UnsafeFilesystemEntry(format!(
                    "{} was not created as a regular directory",
                    path.display()
                )));
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn sorted_directory_entries(path: &Path) -> Result<Vec<fs::DirEntry>, CatalogError> {
    let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries)
}

fn parse_reference_bytes(bytes: &[u8]) -> Result<ContentDigest, CatalogError> {
    let text = std::str::from_utf8(bytes).map_err(|_| CatalogError::InvalidField {
        field: "catalog reference",
        reason: "must be UTF-8".into(),
    })?;
    let canonical = text
        .strip_suffix('\n')
        .ok_or_else(|| CatalogError::InvalidField {
            field: "catalog reference",
            reason: "must end with one newline".into(),
        })?;
    if canonical.contains(char::is_whitespace) {
        return Err(CatalogError::InvalidField {
            field: "catalog reference",
            reason: "must contain only one canonical digest".into(),
        });
    }
    let digest = ContentDigest::from_str(canonical)?;
    if digest.to_hex() != canonical {
        return Err(CatalogError::InvalidField {
            field: "catalog reference",
            reason: "digest must use lowercase hexadecimal".into(),
        });
    }
    Ok(digest)
}

fn read_limited_regular_file(path: &Path, limit: usize) -> Result<Vec<u8>, CatalogError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(CatalogError::UnsafeFilesystemEntry(format!(
            "{} must be a regular non-symlink file",
            path.display()
        )));
    }
    let declared_length = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
    if declared_length > limit {
        return Err(CatalogError::ResourceLimit {
            resource: "catalog file",
            limit,
            actual: declared_length,
        });
    }
    let file = File::open(path)?;
    let mut bytes = Vec::with_capacity(declared_length);
    file.take((limit as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(CatalogError::ResourceLimit {
            resource: "catalog file",
            limit,
            actual: bytes.len(),
        });
    }
    Ok(bytes)
}

enum CreateOutcome {
    Created,
    Identical,
    Different(Vec<u8>),
}

fn atomic_create_or_verify(path: &Path, bytes: &[u8], limit: usize) -> Result<(), CatalogError> {
    match atomic_create_or_compare(path, bytes, limit)? {
        CreateOutcome::Created | CreateOutcome::Identical => Ok(()),
        CreateOutcome::Different(_) => Err(CatalogError::UnsafeFilesystemEntry(format!(
            "existing content-addressed object {} has different bytes",
            path.display()
        ))),
    }
}

/// Publishes by hard-linking a fully written temporary inode. `hard_link` is
/// intentionally used instead of overwrite-capable `rename`: destination
/// creation is atomic and fails when another writer won the race.
fn atomic_create_or_compare(
    path: &Path,
    bytes: &[u8],
    limit: usize,
) -> Result<CreateOutcome, CatalogError> {
    if bytes.len() > limit {
        return Err(CatalogError::ResourceLimit {
            resource: "catalog atomic write",
            limit,
            actual: bytes.len(),
        });
    }
    if let Ok(existing) = read_limited_regular_file(path, limit) {
        return Ok(if existing == bytes {
            CreateOutcome::Identical
        } else {
            CreateOutcome::Different(existing)
        });
    } else if path.exists() {
        // Re-read to preserve the precise unsafe-file/corruption diagnostic.
        let _ = read_limited_regular_file(path, limit)?;
    }

    let parent = path.parent().ok_or_else(|| {
        CatalogError::UnsafeFilesystemEntry("catalog destination has no parent".into())
    })?;
    ensure_directory(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| CatalogError::UnsafeFilesystemEntry("invalid destination name".into()))?;
    let unique = TEMPORARY_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        unique
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    if let Err(error) = (|| -> Result<(), std::io::Error> {
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    })() {
        drop(file);
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    drop(file);

    match fs::hard_link(&temporary, path) {
        Ok(()) => {
            fs::remove_file(&temporary)?;
            sync_directory(parent)?;
            Ok(CreateOutcome::Created)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            fs::remove_file(&temporary)?;
            let existing = read_limited_regular_file(path, limit)?;
            Ok(if existing == bytes {
                CreateOutcome::Identical
            } else {
                CreateOutcome::Different(existing)
            })
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(error.into())
        }
    }
}

/// Replaces a derived file atomically: the new bytes are written and synced
/// under a temporary name, then renamed over the old file.
fn atomic_replace(path: &Path, bytes: &[u8], limit: usize) -> Result<(), CatalogError> {
    if bytes.len() > limit {
        return Err(CatalogError::ResourceLimit {
            resource: "catalog atomic write",
            limit,
            actual: bytes.len(),
        });
    }
    if let Ok(metadata) = fs::symlink_metadata(path)
        && !metadata.file_type().is_file()
    {
        return Err(CatalogError::UnsafeFilesystemEntry(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    let parent = path.parent().ok_or_else(|| {
        CatalogError::UnsafeFilesystemEntry("catalog destination has no parent".into())
    })?;
    ensure_directory(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| CatalogError::UnsafeFilesystemEntry("invalid destination name".into()))?;
    let unique = TEMPORARY_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        unique
    ));
    let written = (|| -> Result<(), std::io::Error> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    sync_directory(parent)
}

/// Flushes a directory entry so a freshly published object survives a crash.
///
/// This is a POSIX durability step: creating a link is not necessarily on disk
/// until the containing directory is synced. Windows has no equivalent —
/// opening a directory as a file is refused outright, which is why publishing
/// failed there with "Access is denied" — and NTFS orders its own metadata, so
/// the step is skipped rather than attempted and failed.
fn sync_directory(path: &Path) -> Result<(), CatalogError> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{
        DisplayUnit, EmbeddedDocument, ParameterId, ParameterSpec, PartDefinition, PartMetadata,
        RealQuantity, RealRules,
    };

    use super::*;

    static TEST_DIRECTORY_COUNTER: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(name: &str) -> Self {
            let unique = TEST_DIRECTORY_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "artificer-catalog-{name}-{}-{unique}",
                std::process::id()
            ));
            if path.exists() {
                fs::remove_dir_all(&path).unwrap();
            }
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            if self.0.exists() {
                fs::remove_dir_all(&self.0).unwrap();
            }
        }
    }

    fn package(id: &str, revision: PartRevision, document_value: &str) -> PartPackage {
        let metadata = PartMetadata::new("Aluminium Extrusion 20 × 20")
            .unwrap()
            .with_category("Extrusions")
            .unwrap()
            .with_material("Aluminium")
            .unwrap()
            .with_tags(["metric", "structural"])
            .unwrap();
        let parameter = ParameterSpec::real(
            ParameterId::parse("length").unwrap(),
            "Length",
            0,
            RealQuantity::Length,
            DisplayUnit::Millimetre,
            None,
            RealRules::new(Some(1.0), Some(6_000.0), Some(1.0)).unwrap(),
        )
        .unwrap();
        let document = EmbeddedDocument::from_json(
            "application/vnd.artificer.native+json",
            2,
            document_value.as_bytes(),
        )
        .unwrap();
        PartPackage::seal(
            PartDefinition::parametric(
                PartDefinitionId::parse(id).unwrap(),
                revision,
                metadata,
                vec![parameter],
                document,
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn publish_load_resolve_rebuild_and_search_round_trip() {
        let directory = TestDirectory::new("round-trip");
        let store = CatalogStore::open(&directory.0).unwrap();
        let package = package(
            "profile-2020",
            PartRevision::new(1, 0, 0),
            r#"{"length":1}"#,
        );

        let digest = store.publish(&package).unwrap();
        assert_eq!(store.publish(&package).unwrap(), digest);
        assert_eq!(store.load(digest).unwrap(), package);
        assert_eq!(
            store
                .resolve(package.definition().id(), PartRevision::new(1, 0, 0))
                .unwrap(),
            package
        );

        let report = store.rebuild_index().unwrap();
        assert_eq!(report.accepted(), 1);
        assert!(report.rejected().is_empty());
        let results = store
            .search(
                &SearchQuery::new("aluminium 20")
                    .with_kind(PartKind::Parametric)
                    .with_category("extrusions")
                    .with_required_tag("METRIC"),
            )
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].digest(), digest);
        assert_eq!(results[0].required_parameter_count(), 1);
    }

    fn preview() -> PartPreview {
        let mut image_png = PNG_SIGNATURE.to_vec();
        image_png.extend_from_slice(b"rest of a png");
        PartPreview {
            image_png,
            facts: PartPreviewFacts {
                extents_mm: [20.0, 20.0, 100.0],
                driven_by: [None, None, Some("Length".into())],
                sample: Some("Length 100 mm".into()),
            },
        }
    }

    #[test]
    fn a_preview_is_kept_beside_its_package_and_can_be_drawn_again() {
        let directory = TestDirectory::new("preview");
        let store = CatalogStore::open(&directory.0).unwrap();
        let package = package(
            "vendor.bracket",
            PartRevision::new(1, 0, 0),
            r#"{"value":1}"#,
        );
        let digest = package.content_digest();
        assert!(matches!(
            store.save_preview(digest, &preview()),
            Err(CatalogError::ObjectNotFound(_))
        ));
        store.publish(&package).unwrap();
        assert_eq!(store.preview(digest).unwrap(), None);

        store.save_preview(digest, &preview()).unwrap();
        assert_eq!(store.preview(digest).unwrap(), Some(preview()));

        let mut redrawn = preview();
        redrawn.image_png.push(1);
        redrawn.facts.sample = Some("Length 50 mm".into());
        store.save_preview(digest, &redrawn).unwrap();
        assert_eq!(store.preview(digest).unwrap(), Some(redrawn));

        // A preview is derived: publishing again and reopening keep it, and
        // the index never mistakes it for a package.
        store.publish(&package).unwrap();
        let reopened = CatalogStore::open(&directory.0).unwrap();
        assert_eq!(reopened.index_snapshot().unwrap().len(), 1);
        assert!(reopened.rejected_snapshot().unwrap().is_empty());
        assert!(reopened.preview(digest).unwrap().is_some());
    }

    #[test]
    fn a_preview_that_is_not_a_png_or_not_sane_is_refused() {
        let directory = TestDirectory::new("preview-refused");
        let store = CatalogStore::open(&directory.0).unwrap();
        let package = package(
            "vendor.bracket",
            PartRevision::new(1, 0, 0),
            r#"{"value":1}"#,
        );
        let digest = store.publish(&package).unwrap();

        let mut not_png = preview();
        not_png.image_png = b"GIF89a".to_vec();
        assert!(store.save_preview(digest, &not_png).is_err());
        let mut bad_extent = preview();
        bad_extent.facts.extents_mm[1] = f64::NAN;
        assert!(store.save_preview(digest, &bad_extent).is_err());
        let mut too_big = preview();
        too_big.image_png.resize(MAX_PREVIEW_IMAGE_BYTES + 1, 0);
        assert!(store.save_preview(digest, &too_big).is_err());
        assert_eq!(store.preview(digest).unwrap(), None);

        // A damaged file on disk reads as no preview, to be drawn again.
        store.save_preview(digest, &preview()).unwrap();
        let (image, _) = store.preview_paths(digest);
        fs::write(&image, b"damaged").unwrap();
        assert_eq!(store.preview(digest).unwrap(), None);
    }

    #[test]
    fn published_revision_is_immutable() {
        let directory = TestDirectory::new("revision-conflict");
        let store = CatalogStore::open(&directory.0).unwrap();
        let first = package("profile-2020", PartRevision::new(1, 0, 0), r#"{"value":1}"#);
        let replacement = package("profile-2020", PartRevision::new(1, 0, 0), r#"{"value":2}"#);
        store.publish(&first).unwrap();

        assert!(matches!(
            store.publish(&replacement),
            Err(CatalogError::RevisionConflict { .. })
        ));
        // A refused publish writes nothing: no unreachable copy of the
        // replacement is left in the object store.
        assert!(!store.object_path(replacement.content_digest()).exists());
        assert_eq!(
            store
                .resolve(first.definition().id(), PartRevision::new(1, 0, 0))
                .unwrap(),
            first
        );
    }

    #[test]
    fn corrupt_object_is_never_returned_and_is_reported_by_rebuild() {
        let directory = TestDirectory::new("corruption");
        let store = CatalogStore::open(&directory.0).unwrap();
        let package = package("profile-2020", PartRevision::new(1, 0, 0), r#"{"value":1}"#);
        let digest = store.publish(&package).unwrap();
        let path = store.object_path(digest);
        fs::write(&path, b"{\"not\":\"a package\"}").unwrap();

        assert!(store.load(digest).is_err());
        let report = store.rebuild_index().unwrap();
        assert_eq!(report.accepted(), 0);
        assert_eq!(report.rejected().len(), 1);
        assert!(store.search(&SearchQuery::default()).unwrap().is_empty());
    }

    #[test]
    fn malformed_reference_does_not_hide_other_valid_entries() {
        let directory = TestDirectory::new("bad-ref");
        let store = CatalogStore::open(&directory.0).unwrap();
        let valid = package("profile-2020", PartRevision::new(1, 0, 0), r#"{"value":1}"#);
        store.publish(&valid).unwrap();
        let bad_directory = directory.0.join(REFERENCES_DIRECTORY).join("bad-part");
        fs::create_dir_all(&bad_directory).unwrap();
        fs::write(bad_directory.join("1.0.0.ref"), b"not-a-digest\n").unwrap();

        let report = store.rebuild_index().unwrap();
        assert_eq!(report.accepted(), 1);
        assert_eq!(report.rejected().len(), 1);
        assert_eq!(store.index_snapshot().unwrap().len(), 1);
    }

    #[test]
    fn concurrent_idempotent_publish_keeps_one_valid_revision() {
        let directory = TestDirectory::new("concurrent");
        let store = Arc::new(CatalogStore::open(&directory.0).unwrap());
        let package = Arc::new(package(
            "profile-2020",
            PartRevision::new(1, 0, 0),
            r#"{"value":1}"#,
        ));
        let expected = package.content_digest();
        let workers = (0..8)
            .map(|_| {
                let store = Arc::clone(&store);
                let package = Arc::clone(&package);
                std::thread::spawn(move || store.publish(&package).unwrap())
            })
            .collect::<Vec<_>>();
        for worker in workers {
            assert_eq!(worker.join().unwrap(), expected);
        }
        let report = store.rebuild_index().unwrap();
        assert_eq!(report.accepted(), 1);
        assert!(report.rejected().is_empty());
        assert_eq!(store.load(expected).unwrap(), *package);
    }

    #[test]
    fn package_bytes_never_embed_catalog_root_path() {
        let directory = TestDirectory::new("path-boundary");
        let store = CatalogStore::open(&directory.0).unwrap();
        let package = package("profile-2020", PartRevision::new(1, 0, 0), r#"{"value":1}"#);
        store.publish(&package).unwrap();
        let bytes = package.to_json_bytes().unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(!text.contains(directory.0.to_string_lossy().as_ref()));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_reference_is_rejected_from_index() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new("symlink");
        let store = CatalogStore::open(&directory.0).unwrap();
        let reference_directory = directory.0.join(REFERENCES_DIRECTORY).join("linked-part");
        fs::create_dir_all(&reference_directory).unwrap();
        let target = directory.0.join("outside.ref");
        fs::write(&target, format!("{}\n", "0".repeat(64))).unwrap();
        symlink(&target, reference_directory.join("1.0.0.ref")).unwrap();

        let report = store.rebuild_index().unwrap();
        assert_eq!(report.accepted(), 0);
        assert_eq!(report.rejected().len(), 1);
        assert!(report.rejected()[0].message().contains("non-symlink"));
    }
}
