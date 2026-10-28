//! Every file of the window's ui/ loads on its own, as Slint's preview opens it, and exports
//! every component it declares, so the preview can show each.

use std::path::{Path, PathBuf};

fn files(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files(&path, found);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "slint")
        {
            found.push(path);
        }
    }
}

#[test]
fn every_ui_file_loads_alone_and_exports_its_components() {
    let mut found = Vec::new();
    files(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("ui"),
        &mut found,
    );
    assert!(found.len() > 10, "{found:?}");
    for path in found {
        let compiled =
            spin_on::spin_on(slint_interpreter::Compiler::default().build_from_path(&path));
        let source = std::fs::read_to_string(&path).unwrap();
        // A file of structs alone has nothing to show, and says so; that's all it may say.
        let structs_only = !source.contains("component ");
        let errors: Vec<String> = compiled
            .diagnostics()
            .filter(|diagnostic| diagnostic.level() == slint_interpreter::DiagnosticLevel::Error)
            .map(|diagnostic| diagnostic.to_string())
            .filter(|error| !(structs_only && error.contains("No component found")))
            .collect();
        assert!(errors.is_empty(), "{}: {errors:#?}", path.display());
        let hidden: Vec<&str> = source
            .lines()
            .filter(|line| line.starts_with("component ") || line.starts_with("global "))
            .collect();
        assert!(
            hidden.is_empty(),
            "{}: not exported: {hidden:?}",
            path.display()
        );
        if !structs_only {
            assert!(
                compiled.component_names().next().is_some(),
                "{}: no component to preview",
                path.display()
            );
        }
    }
}
