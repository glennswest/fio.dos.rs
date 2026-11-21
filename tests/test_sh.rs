//! `test/test.sh`'s cleanup, with the loop and mount commands mocked
//! (f1e178d2225e).
//!
//! The pod test attaches loop devices; its EXIT trap must detach every one it
//! still holds, on an error path or a signal, and none twice. The scenarios
//! are in `tests/sh/test-sh-cleanup.sh`, which needs only bash: no root, no
//! loop driver, nothing is attached for real.

use std::path::Path;
use std::process::Command;

#[test]
fn the_pod_tests_exit_trap_detaches_what_it_attached() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("bash")
        .arg(root.join("tests/sh/test-sh-cleanup.sh"))
        .arg(root.join("test/test.sh"))
        .output()
        .expect("bash runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "cleanup scenarios failed:\n{stdout}\n{stderr}"
    );
    assert_eq!(stdout.matches("ok ").count(), 4, "{stdout}");
}
