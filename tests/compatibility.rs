use fschem::{
    document::{Document, RecordKind},
    library::LibraryPaths,
    scene::{LoadedDocument, Shape},
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "fixture-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn write(&self, relative: &str, content: &str) -> PathBuf {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn local_symbols_win_and_library_bare_names_resolve_with_spaces_in_paths() {
    let fixture = Fixture::new();
    let document = fixture.write(
        "project with spaces/circuit.sch",
        "v {xschem file_version=1.3}\nC {part.sym} 0 0 0 0 {}\nC {res.sym} 100 0 0 0 {}\n",
    );
    let local = fixture.write(
        "project with spaces/part.sym",
        "K {type=test}\nL 4 0 0 10 0 {}\n",
    );
    fixture.write("library/part.sym", "K {type=test}\nL 4 0 0 999 0 {}\n");
    let resistor = fixture.write(
        "library/devices/res.sym",
        "v {xschem file_version=1.3}\nL 4 0 -10 0 10 {}\n",
    );
    let paths = LibraryPaths {
        roots: vec![fixture.0.join("library")],
    };
    let loaded = LoadedDocument::open(&document, paths).unwrap();
    assert!(loaded.scene.diagnostics.is_empty());
    assert_eq!(
        loaded.scene.dependencies,
        vec![
            fs::canonicalize(local).unwrap(),
            fs::canonicalize(resistor).unwrap()
        ]
    );
    assert!(loaded.scene.bounds.max.x < 200.);
}

#[test]
fn missing_symbols_and_cycles_are_visible_not_fatal() {
    let fixture = Fixture::new();
    let root = fixture.write(
        "a.sch",
        "v {xschem file_version=1.3}\nC {loop.sym} 0 0 0 0 {}\nC {missing.sym} 80 0 0 0 {}\n",
    );
    fixture.write(
        "loop.sym",
        "v {xschem file_version=1.3}\nC {loop.sym} 0 0 0 0 {}\n",
    );
    let loaded = LoadedDocument::open(&root, LibraryPaths::default()).unwrap();
    assert!(
        loaded
            .scene
            .diagnostics
            .iter()
            .any(|d| d.message.contains("cyclic"))
    );
    assert!(
        loaded
            .scene
            .diagnostics
            .iter()
            .any(|d| d.message.contains("missing symbol"))
    );
    assert!(loaded.scene.items.iter().any(|i| i.layer == 7));
    assert!(loaded.scene.items.len() < 20);
}

#[test]
fn legacy_v1_references_and_global_symbol_metadata_are_supported() {
    let fixture = Fixture::new();
    let root = fixture.write("old.sch", "C {part} 0 0 0 0 {name=R5}\n");
    fixture.write("part.sym", "G {type=resistor template=\"value=3k\"}\nK {}\nL 4 0 0 10 0 {}\nT {@name @value} 0 10 0 0 .2 .2 {}\n");
    let loaded = LoadedDocument::open(&root, LibraryPaths::default()).unwrap();
    assert!(loaded.scene.diagnostics.is_empty());
    assert!(
        loaded
            .scene
            .items
            .iter()
            .any(|i| matches!(&i.shape, Shape::Text(t) if t.text == "R5 3k"))
    );
}

#[test]
fn executable_content_is_retained_without_running_it() {
    let source = "v {xschem file_version=1.3}\nK {custom=\"literal [exec command]\"}\nT {tcleval([exec command])} 0 0 0 0 .2 .2 {}\nC {generator.tcl(42)} 0 20 0 0 {}\n";
    let doc = Arc::new(Document::parse(source).unwrap());
    let loaded =
        LoadedDocument::from_document(Arc::clone(&doc), None, LibraryPaths::default()).unwrap();
    assert_eq!(doc.original(), source);
    assert_eq!(
        doc.global('K').unwrap().get("custom"),
        Some("literal [exec command]")
    );
    assert!(
        loaded
            .scene
            .diagnostics
            .iter()
            .any(|d| d.message.contains("generated/Tcl"))
    );
    assert!(
        loaded
            .scene
            .items
            .iter()
            .any(|i| matches!(&i.shape, Shape::Text(t) if t.text == "[Tcl expression]"))
    );
}

#[test]
fn comments_can_separate_instances_from_embedded_definitions() {
    let source = "v {xschem file_version=1.3}\nC {embedded.sym} 0 0 0 0 {}\n# definition follows\n[\nL 4 0 0 20 0 {}\n]\n";
    let doc = Arc::new(Document::parse(source).unwrap());
    assert!(doc.diagnostics.is_empty());
    let loaded = LoadedDocument::from_document(doc, None, LibraryPaths::default()).unwrap();
    assert!(loaded.scene.instances[0].embedded);
    assert!(loaded.scene.diagnostics.is_empty());
    assert_eq!(loaded.scene.items.len(), 1);
}

#[test]
fn unknown_fields_and_duplicate_properties_survive_decoding() {
    let source = "v {xschem file_version=9.0}\r\nZ {future data}\r\nC {a.sym} 0 0 0 0 {name=R1 value=1k value=2k future=\"a b\"}\r\n";
    let doc = Document::parse(source).unwrap();
    let RecordKind::Instance { properties, .. } = &doc.records[2].kind else {
        panic!("instance");
    };
    assert_eq!(properties.get("value"), Some("1k"));
    assert_eq!(
        properties
            .entries
            .iter()
            .filter(|(key, _)| key == "value")
            .count(),
        2
    );
    assert_eq!(properties.get("future"), Some("a b"));
    assert_eq!(doc.raw_record(&doc.records[1]), "Z {future data}\r\n");
    assert_eq!(doc.original().as_bytes(), source.as_bytes());
}

#[test]
#[ignore = "requires XSCHEM_TEST_LIBRARY pointing to an external Xschem library"]
fn installed_library_scenes() {
    fn visit(dir: &Path, files: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                visit(&entry.path(), files);
            } else if entry
                .path()
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("sch") || e.eq_ignore_ascii_case("sym"))
            {
                files.push(entry.path());
            }
        }
    }
    let root =
        PathBuf::from(std::env::var_os("XSCHEM_TEST_LIBRARY").expect("set XSCHEM_TEST_LIBRARY"));
    let root = fs::canonicalize(root).unwrap();
    let paths = LibraryPaths {
        roots: vec![root.clone()],
    };
    let mut files = Vec::new();
    visit(&root, &mut files);
    assert!(!files.is_empty());
    let (mut items, mut notices) = (0, 0);
    for file in &files {
        let loaded = LoadedDocument::open(file, paths.clone())
            .unwrap_or_else(|error| panic!("{}: {error}", file.display()));
        for item in &loaded.scene.items {
            assert!(
                [
                    item.bounds.min.x,
                    item.bounds.min.y,
                    item.bounds.max.x,
                    item.bounds.max.y
                ]
                .iter()
                .all(|v| v.is_finite()),
                "{}",
                file.display()
            );
        }
        items += loaded.scene.items.len();
        notices += loaded.scene.diagnostics.len();
    }
    println!(
        "Loaded {} scenes: {items} drawing items, {notices} compatibility notices",
        files.len()
    );
}
