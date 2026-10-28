//! Load a directory tree of model packages through the real preset catalog and
//! print what the product would see for each one.
//!
//! ```text
//! cargo run -p bongocat-model --example preset_probe -- <catalog-root>
//! ```
//!
//! Exists so a third-party skin folder can be checked before it is committed:
//! the settings window is the only other place that shows whether a package is
//! ready or broken, and driving it costs a full build.

use bongocat_model::{ModelCatalogEntry, ModelPackageLimits, PresetModelCatalog};
use std::{
    io::{self, Write},
    process::ExitCode,
};

fn main() -> ExitCode {
    let Some(root) = std::env::args().nth(1) else {
        let _ = writeln!(io::stderr(), "usage: preset_probe <catalog-root>");
        return ExitCode::FAILURE;
    };
    let catalog = match PresetModelCatalog::open(&root, ModelPackageLimits::default()) {
        Ok(catalog) => catalog,
        Err(error) => {
            let _ = writeln!(io::stderr(), "catalog rejected: {error:?}");
            return ExitCode::FAILURE;
        }
    };
    let entries = match catalog.list() {
        Ok(entries) => entries,
        Err(error) => {
            let _ = writeln!(io::stderr(), "listing failed: {error:?}");
            return ExitCode::FAILURE;
        }
    };

    let mut ready = 0usize;
    let mut broken = 0usize;
    let mut stdout = io::stdout();
    for entry in &entries {
        match entry {
            ModelCatalogEntry::Ready { snapshot, .. } => {
                ready += 1;
                let _ = writeln!(
                    stdout,
                    "READY  {:<24} entry={:<28} behaviors={}",
                    snapshot.id.as_str(),
                    snapshot.entry,
                    snapshot.behaviors.len()
                );
            }
            ModelCatalogEntry::Invalid { id, code, resource, detail, .. } => {
                broken += 1;
                let _ = writeln!(
                    stdout,
                    "BROKEN {:<24} {code:?} resource={resource:?} {detail}",
                    id.as_str()
                );
            }
        }
    }
    let _ = writeln!(stdout, "\n{ready} ready, {broken} broken");
    ExitCode::SUCCESS
}
