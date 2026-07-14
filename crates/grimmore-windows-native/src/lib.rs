//! Audited Windows-only primitives for the private Grimmore named pipe.
//!
//! This crate is the sole exception to the workspace-wide `unsafe_code` ban.
//! It owns the small FFI boundary required to build a current-user DACL and to
//! compare named-pipe peer SIDs. All consumers receive safe, checked helpers.

mod pipe_name;

pub use pipe_name::{pipe_endpoint_for_sid, validate_local_named_pipe_name};

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::{
    create_current_user_pipe, current_user_pipe_endpoint, named_pipe_client_is_current_user,
    named_pipe_server_is_current_user,
};
