//! Connection teardown releases client resources, not primary execution.
use super::{ClientConnectionInfo, ClientDebugState, SwarmMember, unregister_session_event_sender};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub(super) struct DepartingClient<'a> {
    pub session_id: &'a str,
    pub debug_id: &'a str,
    pub connection_id: &'a str,
}

pub(super) async fn cleanup_client_connection(
    client: DepartingClient<'_>,
    event_handle: tokio::task::JoinHandle<()>,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    client_debug_state: &Arc<RwLock<ClientDebugState>>,
    client_connections: &Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    startup_context: &super::startup_context::StartupContextCoordinator,
) -> Result<()> {
    let DepartingClient {
        session_id: client_session_id,
        debug_id: client_debug_id,
        connection_id: client_connection_id,
    } = client;
    startup_context.release_connection(client_connection_id);
    client_debug_state.write().await.unregister(client_debug_id);
    client_connections
        .write()
        .await
        .remove(client_connection_id);
    unregister_session_event_sender(swarm_members, client_session_id, client_connection_id).await;
    event_handle.abort();
    let _ = event_handle.await;
    Ok(())
}
