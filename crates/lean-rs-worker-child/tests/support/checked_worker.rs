//! Digest check for a worker binary the qualifier selected.
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

/// Asserts that `worker` is a regular file of at most 256 MiB whose SHA-256
/// digest is `expected`, so a test launches only the binary the qualifier sealed.
pub(super) fn checked_worker(worker: &Path, expected: &str) {
    use sha2::{Digest, Sha256};
    use std::io::Read as _;
    let mut file = fs::File::open(worker).expect("open selected worker");
    let metadata = file.metadata().expect("selected worker metadata");
    assert!(metadata.is_file() && metadata.len() <= 256 * 1024 * 1024);
    let mut remaining = metadata.len();
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let count = file.read(&mut buffer).expect("read selected worker");
        if count == 0 {
            break;
        }
        remaining = remaining.checked_sub(count as u64).expect("selected worker grew");
        digest.update(buffer.get(..count).expect("read count fits buffer"));
    }
    assert_eq!(remaining, 0, "selected worker shrank");
    let mut actual = String::new();
    for byte in digest.finalize() {
        write!(&mut actual, "{byte:02x}").expect("digest text");
    }
    assert_eq!(actual, expected, "selected fixture worker changed");
}
