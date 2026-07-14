//! Content revisions used to prevent stale vault writes.

use sha2::{Digest, Sha256};

/// Returns a portable, algorithm-qualified revision for UTF-8 note content.
#[must_use]
pub fn content_revision(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    format!("sha256:{}", hex::encode(digest))
}

#[cfg(test)]
mod tests {
    use super::content_revision;

    #[test]
    fn revision_is_deterministic_and_algorithm_qualified() {
        assert_eq!(
            content_revision("Grimmore"),
            "sha256:8b77e81385d6b77d67bb9ea2cb6b269f0401d05f9fa5d9877d75fa28aca9f77b"
        );
    }
}
