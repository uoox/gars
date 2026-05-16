// Ensures the embedded extension zip exists at build time. CI populates it
// with the real bundle (extension/dist.zip from `npm run build`); local dev
// gets a tiny placeholder so `cargo build` still works without Node.

use std::fs;
use std::path::Path;

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let workspace = Path::new(&manifest)
        .parent()
        .and_then(|p| p.parent())
        .unwrap();

    let ext_dist_zip = workspace.join("extension").join("dist.zip");
    if !ext_dist_zip.exists() {
        if let Some(parent) = ext_dist_zip.parent() {
            fs::create_dir_all(parent).ok();
        }
        // Minimal empty ZIP: PK end-of-central-directory record only.
        let eocd = [
            0x50u8, 0x4b, 0x05, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        fs::write(&ext_dist_zip, eocd).expect("write placeholder dist.zip");
    }

    println!("cargo:rerun-if-changed={}", ext_dist_zip.display());
}
