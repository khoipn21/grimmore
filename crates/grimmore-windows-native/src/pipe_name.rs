use std::{
    ffi::{OsStr, OsString},
    io,
};

const LOCAL_PIPE_PREFIX: &str = r"\\.\pipe\";

/// Confirms that a pipe name is in Windows' local-only named-pipe namespace.
pub fn validate_local_named_pipe_name(name: &OsStr) -> io::Result<()> {
    let Some(name) = name.to_str() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "named-pipe endpoint is not valid Unicode text",
        ));
    };
    let suffix = name.strip_prefix(LOCAL_PIPE_PREFIX).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "named-pipe endpoint must use the local \\.\\pipe\\ namespace",
        )
    })?;
    if suffix.is_empty() || suffix.contains(['\\', '/']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "named-pipe endpoint must contain one non-empty local pipe name",
        ));
    }
    Ok(())
}

/// Derives a stable, local-only pipe endpoint for one Windows user SID.
pub fn pipe_endpoint_for_sid(pipe_name: &str, sid: &str) -> io::Result<OsString> {
    if pipe_name.is_empty() || pipe_name.contains(['\\', '/']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "named-pipe base name must be one non-empty component",
        ));
    }
    if !is_windows_sid(sid) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows returned an invalid SID string",
        ));
    }
    let endpoint = OsString::from(format!("{LOCAL_PIPE_PREFIX}{pipe_name}-{sid}"));
    validate_local_named_pipe_name(&endpoint)?;
    Ok(endpoint)
}

pub(crate) fn is_windows_sid(sid: &str) -> bool {
    let Some(parts) = sid.strip_prefix("S-") else {
        return false;
    };
    !parts.is_empty()
        && parts
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'-')
}

#[cfg(test)]
mod tests {
    use super::{pipe_endpoint_for_sid, validate_local_named_pipe_name};

    #[test]
    fn per_user_endpoints_are_distinct_and_stable() {
        let first = pipe_endpoint_for_sid("grimmore-v1", "S-1-5-21-100-200-300-400")
            .expect("first endpoint is valid");
        let again = pipe_endpoint_for_sid("grimmore-v1", "S-1-5-21-100-200-300-400")
            .expect("repeat endpoint is valid");
        let second = pipe_endpoint_for_sid("grimmore-v1", "S-1-5-21-500-600-700-800")
            .expect("second endpoint is valid");

        assert_eq!(first, again);
        assert_ne!(first, second);
        validate_local_named_pipe_name(&first).expect("first endpoint remains local-only");
        validate_local_named_pipe_name(&second).expect("second endpoint remains local-only");
    }

    #[test]
    fn rejects_unsafe_pipe_components_and_sid_text() {
        assert!(pipe_endpoint_for_sid("", "S-1-5-21-100").is_err());
        assert!(pipe_endpoint_for_sid("nested\\pipe", "S-1-5-21-100").is_err());
        assert!(pipe_endpoint_for_sid("grimmore-v1", "not-a-sid").is_err());
    }
}
