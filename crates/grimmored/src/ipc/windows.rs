use std::{future::Future, os::windows::io::AsRawHandle as _, path::Path};

use grimmore_windows_native::{
    create_current_user_pipe, named_pipe_client_is_current_user, validate_local_named_pipe_name,
};
use tokio::net::windows::named_pipe::NamedPipeServer;
use tracing::{debug, warn};

use super::{IpcServerConfig, IpcServerError, connection::handle_connection};

pub async fn serve(config: IpcServerConfig) -> Result<(), IpcServerError> {
    serve_with_shutdown(config, async {
        let mut ctrl_c = match tokio::signal::windows::ctrl_c() {
            Ok(signal) => signal,
            Err(error) => {
                warn!(%error, "failed to install Ctrl-C IPC shutdown signal");
                return;
            }
        };
        let mut ctrl_break = match tokio::signal::windows::ctrl_break() {
            Ok(signal) => signal,
            Err(error) => {
                warn!(%error, "failed to install Ctrl-Break IPC shutdown signal");
                return;
            }
        };
        tokio::select! {
            _ = ctrl_c.recv() => {},
            _ = ctrl_break.recv() => {},
        }
    })
    .await
}

pub async fn serve_with_shutdown<F>(
    config: IpcServerConfig,
    shutdown: F,
) -> Result<(), IpcServerError>
where
    F: Future<Output = ()>,
{
    validate_local_named_pipe_name(config.endpoint.as_os_str())
        .map_err(|_| IpcServerError::UntrustedEndpoint(config.endpoint.clone()))?;
    let mut listener = create_listener(&config.endpoint, true)?;
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            () = &mut shutdown => return Ok(()),
            connected = listener.connect() => {
                connected.map_err(|source| IpcServerError::Io {
                    operation: "accept named-pipe client",
                    path: config.endpoint.clone(),
                    source,
                })?;
                let stream = listener;
                listener = create_listener(&config.endpoint, false)?;

                match named_pipe_client_is_current_user(stream.as_raw_handle() as usize) {
                    Ok(true) => {},
                    Ok(false) => {
                        warn!("rejected named-pipe peer owned by another Windows user");
                        continue;
                    }
                    Err(error) => {
                        warn!(%error, "rejected named-pipe peer without a verifiable process SID");
                        continue;
                    }
                }

                let connection_config = config.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(stream, connection_config).await {
                        debug!(%error, "IPC connection closed with an error");
                    }
                });
            }
        }
    }
}

fn create_listener(
    endpoint: &Path,
    first_instance: bool,
) -> Result<NamedPipeServer, IpcServerError> {
    create_current_user_pipe(endpoint.as_os_str(), first_instance).map_err(|source| {
        IpcServerError::Io {
            operation: if first_instance {
                "create private named-pipe endpoint"
            } else {
                "create next named-pipe instance"
            },
            path: endpoint.to_path_buf(),
            source,
        }
    })
}
