use std::path::Path;

fn main() {
    // Locate the peripheral crate source directory relative to this crate's
    // manifest.  During development `CARGO_MANIFEST_DIR` points to
    // `crates/boot/`, so `../../crates/peripheral` yields the peripheral
    // crate root.
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by Cargo");
    let crates_dir = Path::new(&manifest_dir).join("..").join("..");

    let seed_source = crates_dir.join("crates").join("peripheral");

    // Canonicalise to an absolute path.  This will fail at build time if
    // the directory doesn't exist, which is the desired behaviour.
    let seed_source = seed_source
        .canonicalize()
        .expect("crates/peripheral must exist at build time");

    println!(
        "cargo:rustc-env=RELOOPY_SEED_SOURCE={}",
        seed_source.display()
    );

    // Re-run this script if the peripheral directory is moved/deleted.
    println!("cargo:rerun-if-changed={}", seed_source.display());

    // Also locate the IPC crate so the seed workspace can include it.
    let seed_ipc = crates_dir.join("crates").join("ipc");
    let seed_ipc = seed_ipc
        .canonicalize()
        .expect("crates/ipc must exist at build time");

    println!(
        "cargo:rustc-env=RELOOPY_SEED_IPC={}",
        seed_ipc.display()
    );
    println!("cargo:rerun-if-changed={}", seed_ipc.display());
}
