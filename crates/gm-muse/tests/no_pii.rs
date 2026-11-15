//! Guard test ensuring bundled Muse assets contain zero real-person PII from upstream leaks.

use std::path::Path;

const FORBIDDEN_PII: &[&str] = &["\u{00C1}sge\u{0069}r", "Asge\u{0069}r", "asge\u{0069}rtj@"];

fn check_dir_recursive(dir: &Path) {
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("Failed to read assets dir {}: {e}", dir.display()));
    for entry in entries {
        let entry = entry.expect("Failed to read directory entry");
        let path = entry.path();
        if path.is_dir() {
            check_dir_recursive(&path);
        } else {
            let bytes = std::fs::read(&path)
                .unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()));
            let text = String::from_utf8_lossy(&bytes);
            for needle in FORBIDDEN_PII {
                assert!(
                    !text.contains(needle),
                    "Forbidden PII token '{needle}' found in {}",
                    path.display()
                );
            }
        }
    }
}

#[test]
fn bundled_assets_contain_no_forbidden_pii() {
    let assets_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    assert!(
        assets_dir.exists(),
        "assets directory missing at {}",
        assets_dir.display()
    );
    check_dir_recursive(&assets_dir);
}
