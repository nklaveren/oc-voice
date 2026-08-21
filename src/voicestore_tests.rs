//! Tests for the voiceprint on disk.

use super::*;

#[cfg(unix)]
#[test]
fn the_voiceprint_is_not_world_readable() {
    // Tested on the mechanism rather than through `save()`. `testing.rs`
    // exists to make writing to the real state directory safe, but the file
    // that would be written here is a biometric, and a test that could
    // overwrite one on a mistake in the isolation is not worth a file mode.
    use std::os::unix::fs::PermissionsExt;
    let p = std::env::temp_dir().join(format!("oc-voice-perm-{}", std::process::id()));
    std::fs::write(&p, "x").expect("scratch file");
    assert_ne!(
        std::fs::metadata(&p).unwrap().permissions().mode() & 0o077,
        0,
        "the default mode is the thing being fixed; if it is already 0600 this proves nothing"
    );
    owner_only(&p);
    let mode = std::fs::metadata(&p).unwrap().permissions().mode();
    assert_eq!(
        mode & 0o077,
        0,
        "mode {:o} lets others read it",
        mode & 0o777
    );
    let _ = std::fs::remove_file(&p);
}
