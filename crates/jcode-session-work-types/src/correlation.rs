//! Contract versions and the reply-correlation rule for session-work clients.
//!
//! The rule belongs to the contract owner so the Harness bridge, both SDKs and
//! later adapters apply the same identity checks. A matching reply is not
//! authority; it only proves the reply describes the request that was sent.
use super::*;

/// Independently negotiated session-work contracts. Later contracts add
/// variants here rather than overloading an existing version.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionWorkCapability {
    /// `session_work_v1`: workflows and the other session-work operations.
    SessionWork,
}

/// The versions a runtime advertised. Absent fields come from runtimes that
/// predate the contract.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionWorkVersions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_work_version: Option<u32>,
}

impl SessionWorkVersions {
    /// The versions this build of the contract serves.
    pub fn current() -> Self {
        Self {
            session_work_version: Some(SESSION_WORK_VERSION),
        }
    }

    /// Only the exact versions this contract describes are supported.
    pub fn supports(&self, capability: SessionWorkCapability) -> bool {
        match capability {
            SessionWorkCapability::SessionWork => {
                self.session_work_version == Some(SESSION_WORK_VERSION)
            }
        }
    }
}

/// A client-chosen request identity. Mutations are idempotent per request:
/// a retried request converges on its original receipt.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(pub uuid::Uuid);

impl RequestId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl Default for RequestId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// The identities every session-work request and reply carry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Correlation {
    pub request: RequestId,
    pub session: String,
}

impl Correlation {
    /// The single correlation rule: a reply describes a request only when every
    /// identity it carries is the request's own. A request ID alone is not
    /// enough, because a reply for the same ID about another session is a
    /// defect, not an answer.
    pub fn matches_reply(&self, reply: &Correlation) -> bool {
        self.request == reply.request && self.session == reply.session
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_exact_version_is_supported() {
        assert!(SessionWorkVersions::current().supports(SessionWorkCapability::SessionWork));
        assert!(!SessionWorkVersions::default().supports(SessionWorkCapability::SessionWork));
        let newer = SessionWorkVersions {
            session_work_version: Some(SESSION_WORK_VERSION + 1),
        };
        assert!(!newer.supports(SessionWorkCapability::SessionWork));
        let decoded: SessionWorkVersions = serde_json::from_str("{}").unwrap();
        assert_eq!(decoded, SessionWorkVersions::default());
    }

    #[test]
    fn replies_match_only_their_own_request_and_session() {
        let request = Correlation {
            request: RequestId::new(),
            session: "session_a".into(),
        };
        assert!(request.matches_reply(&request.clone()));
        let other_session = Correlation {
            session: "session_b".into(),
            ..request.clone()
        };
        assert!(!request.matches_reply(&other_session));
        let other_request = Correlation {
            request: RequestId::new(),
            ..request.clone()
        };
        assert!(!request.matches_reply(&other_request));
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(serde_json::from_str::<Correlation>(&json).unwrap(), request);
    }
}
