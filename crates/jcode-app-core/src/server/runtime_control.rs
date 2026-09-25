//! Administrative runtime controls never construct or attach a primary Session.
use crate::protocol::{Request, ServerEvent};
use crate::workspace::{Issue, IssueCode, runtime::RuntimeResponse};
#[cfg(unix)]
use std::sync::Arc;

pub(super) async fn dispatch(
    request: &Request,
    #[cfg(unix)] lifecycle: Option<&Arc<super::shutdown::RuntimeLifecycle>>,
) -> Option<ServerEvent> {
    #[cfg(unix)]
    let version = lifecycle.map(|_| 1);
    #[cfg(not(unix))]
    let version = None;
    match request {
        Request::RuntimeProbe { id } => Some(ServerEvent::RuntimeCapabilities { id: *id, version }),
        Request::RuntimeControl { id, request } => {
            #[cfg(unix)]
            let result = match lifecycle {
                Some(lifecycle) => lifecycle.request(*request.clone()).await,
                None => Err(anyhow::anyhow!(
                    "This server has no runtime lifecycle owner"
                )),
            };
            #[cfg(not(unix))]
            let result: anyhow::Result<RuntimeResponse> = {
                let _ = request;
                Err(anyhow::anyhow!(
                    "Reviewed runtime control is not supported on this platform"
                ))
            };
            let response = result.unwrap_or_else(|error| {
                RuntimeResponse::Error(error.downcast_ref::<Issue>().cloned().unwrap_or_else(
                    || Issue {
                        code: if version.is_some() {
                            IssueCode::RecoveryRequired
                        } else {
                            IssueCode::UnsupportedCapability
                        },
                        detail: format!("{error:#}"),
                    },
                ))
            });
            Some(ServerEvent::RuntimeResponse {
                id: *id,
                response: Box::new(response),
            })
        }
        _ => None,
    }
}
