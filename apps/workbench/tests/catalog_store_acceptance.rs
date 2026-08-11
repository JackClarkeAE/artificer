use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use artificer_catalog::{CatalogError, CatalogStore, PartKind, SearchQuery};
use artificer_workbench::library_catalog::builtin_aluminium_extrusion_package;

static TEST_DIRECTORY_COUNTER: AtomicU64 = AtomicU64::new(1);

#[test]
fn builtin_package_survives_the_complete_catalog_lifecycle() {
    let roots = TestStoreRoots::new();
    let first_authored = builtin_aluminium_extrusion_package().unwrap();
    let second_authored = builtin_aluminium_extrusion_package().unwrap();
    let expected_bytes = first_authored.to_json_bytes().unwrap();
    let expected_digest = first_authored.content_digest();
    let definition_id = first_authored.definition().id().clone();
    let revision = first_authored.definition().revision();

    assert_eq!(
        first_authored.content_digest(),
        second_authored.content_digest()
    );
    assert_eq!(expected_bytes, second_authored.to_json_bytes().unwrap());

    let store = CatalogStore::open(&roots.primary).unwrap();
    assert_eq!(store.publish(&first_authored).unwrap(), expected_digest);
    let rebuilt = store.rebuild_index().unwrap();
    assert_eq!(rebuilt.accepted(), 1);
    assert!(rebuilt.rejected().is_empty());

    let matches = store
        .search(
            &SearchQuery::new("20 aluminium")
                .with_kind(PartKind::Parametric)
                .with_category("structural / aluminium extrusion")
                .with_required_tag("PARAMETRIC"),
        )
        .unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].definition_id(), &definition_id);
    assert_eq!(matches[0].revision(), revision);
    assert_eq!(matches[0].digest(), expected_digest);
    assert_eq!(matches[0].parameter_count(), 1);
    assert_eq!(matches[0].required_parameter_count(), 1);

    let exact = store.resolve(&definition_id, revision).unwrap();
    assert_eq!(exact.content_digest(), expected_digest);
    assert_eq!(exact.to_json_bytes().unwrap(), expected_bytes);
    drop(store);

    let reopened = CatalogStore::open(&roots.primary).unwrap();
    assert_eq!(reopened.index_snapshot().unwrap().len(), 1);
    assert!(reopened.rejected_snapshot().unwrap().is_empty());
    let reopened_exact = reopened.resolve(&definition_id, revision).unwrap();
    assert_eq!(reopened_exact.content_digest(), expected_digest);
    assert_eq!(reopened_exact.to_json_bytes().unwrap(), expected_bytes);

    let other_store = CatalogStore::open(&roots.secondary).unwrap();
    assert_eq!(
        other_store.publish(&second_authored).unwrap(),
        expected_digest
    );
    let primary_object = only_object_file(&roots.primary);
    let secondary_object = only_object_file(&roots.secondary);
    assert_eq!(
        primary_object.strip_prefix(&roots.primary).unwrap(),
        secondary_object.strip_prefix(&roots.secondary).unwrap(),
        "content addressing must not depend on the store's absolute path"
    );
    assert_eq!(
        fs::read(primary_object).unwrap(),
        fs::read(secondary_object).unwrap(),
        "the same authored definition must publish byte-for-byte identically"
    );

    let corrupt_store = CatalogStore::open(&roots.corrupt).unwrap();
    corrupt_store.publish(&first_authored).unwrap();
    let corrupt_object = only_object_file(&roots.corrupt);
    let mut corrupt_bytes = fs::read(&corrupt_object).unwrap();
    let marker = b"Aluminium";
    let marker_start = corrupt_bytes
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("the built-in package metadata contains its material");
    corrupt_bytes[marker_start] = b'B';
    fs::write(&corrupt_object, corrupt_bytes).unwrap();

    let corrupt_resolution = corrupt_store.resolve(&definition_id, revision);
    assert!(
        matches!(corrupt_resolution, Err(CatalogError::DigestMismatch { .. })),
        "unexpected corruption diagnostic: {corrupt_resolution:?}"
    );
    let corrupt_report = corrupt_store.rebuild_index().unwrap();
    assert_eq!(corrupt_report.accepted(), 0);
    assert_eq!(corrupt_report.rejected().len(), 1);
    assert!(
        corrupt_store
            .search(&SearchQuery::default())
            .unwrap()
            .is_empty()
    );
    drop(corrupt_store);

    let reopened_corrupt = CatalogStore::open(&roots.corrupt).unwrap();
    assert!(reopened_corrupt.index_snapshot().unwrap().is_empty());
    assert_eq!(reopened_corrupt.rejected_snapshot().unwrap().len(), 1);
    assert!(matches!(
        reopened_corrupt.resolve(&definition_id, revision),
        Err(CatalogError::DigestMismatch { .. })
    ));
}

fn only_object_file(root: &Path) -> PathBuf {
    let objects = root.join("objects");
    let files = regular_files_below(&objects);
    assert_eq!(files.len(), 1, "expected one content-addressed object");
    files.into_iter().next().unwrap()
}

fn regular_files_below(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let mut entries = fs::read_dir(directory)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let file_type = entry.file_type().unwrap();
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file() {
                files.push(entry.path());
            }
        }
    }
    files.sort();
    files
}

struct TestStoreRoots {
    base: PathBuf,
    primary: PathBuf,
    secondary: PathBuf,
    corrupt: PathBuf,
}

impl TestStoreRoots {
    fn new() -> Self {
        let unique = TEST_DIRECTORY_COUNTER.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!(
            "artificer-catalog-acceptance-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&base).unwrap();
        Self {
            primary: base.join("primary"),
            secondary: base.join("secondary"),
            corrupt: base.join("corrupt"),
            base,
        }
    }
}

impl Drop for TestStoreRoots {
    fn drop(&mut self) {
        let temporary_root = std::env::temp_dir();
        assert!(self.base.starts_with(&temporary_root));
        let _ = fs::remove_dir_all(&self.base);
    }
}
