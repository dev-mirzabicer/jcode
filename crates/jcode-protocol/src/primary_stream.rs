//! Optional ordered presentation metadata. It does not alter ServerEvent payloads.
use crate::ServerEvent;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrimaryStreamCursor {
    pub session_id: String,
    pub stream_id: String,
    pub sequence: u64,
    pub request_id: u64,
    pub origin: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum PrimaryStreamPosition {
    Live {
        cursor: PrimaryStreamCursor,
    },
    Snapshot {
        cursor: PrimaryStreamCursor,
        replay_events: usize,
    },
    Replay {
        cursor: PrimaryStreamCursor,
        index: usize,
    },
}

#[derive(Default)]
pub struct PrimaryStreamTracker {
    cursor: Option<PrimaryStreamCursor>,
    replay: Option<(usize, usize)>,
}

impl PrimaryStreamTracker {
    pub fn observe(&mut self, position: &PrimaryStreamPosition) -> Result<(), &'static str> {
        match position {
            PrimaryStreamPosition::Snapshot {
                cursor,
                replay_events,
            } => {
                self.cursor = Some(cursor.clone());
                self.replay = (*replay_events > 0).then_some((0, *replay_events));
            }
            PrimaryStreamPosition::Replay { cursor, index } => {
                let Some((next, total)) = self.replay else {
                    return Err("Unexpected primary snapshot replay");
                };
                if self.cursor.as_ref() != Some(cursor) || *index != next {
                    return Err("Primary snapshot replay has a gap");
                }
                self.replay = (next + 1 < total).then_some((next + 1, total));
            }
            PrimaryStreamPosition::Live { cursor } => {
                if self.replay.is_some() {
                    return Err("Primary snapshot replay is incomplete");
                }
                if let Some(previous) = &self.cursor
                    && (cursor.session_id != previous.session_id
                        || cursor.stream_id != previous.stream_id
                        || previous.sequence.checked_add(1) != Some(cursor.sequence))
                {
                    return Err("Primary live stream needs a fresh snapshot");
                }
                self.cursor = Some(cursor.clone());
            }
        }
        Ok(())
    }
}

pub fn encode_primary_event(
    event: &ServerEvent,
    position: &PrimaryStreamPosition,
) -> serde_json::Result<String> {
    #[derive(Serialize)]
    struct Frame<'a> {
        #[serde(flatten)]
        event: &'a ServerEvent,
        primary_stream: &'a PrimaryStreamPosition,
    }
    let mut frame = serde_json::to_string(&Frame {
        event,
        primary_stream: position,
    })?;
    frame.push('\n');
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gaps_duplicates_and_missing_replay_require_a_new_snapshot() {
        let mut tracker = PrimaryStreamTracker::default();
        let mut cursor = PrimaryStreamCursor {
            session_id: "session".into(),
            stream_id: "stream".into(),
            sequence: 8,
            request_id: 1,
            origin: None,
        };
        tracker
            .observe(&PrimaryStreamPosition::Snapshot {
                cursor: cursor.clone(),
                replay_events: 2,
            })
            .unwrap();
        tracker
            .observe(&PrimaryStreamPosition::Replay {
                cursor: cursor.clone(),
                index: 0,
            })
            .unwrap();
        assert!(
            tracker
                .observe(&PrimaryStreamPosition::Live {
                    cursor: cursor.clone()
                })
                .is_err()
        );
        assert!(
            tracker
                .observe(&PrimaryStreamPosition::Replay {
                    cursor: cursor.clone(),
                    index: 0
                })
                .is_err()
        );
        tracker
            .observe(&PrimaryStreamPosition::Replay {
                cursor: cursor.clone(),
                index: 1,
            })
            .unwrap();
        cursor.sequence = 10;
        assert!(
            tracker
                .observe(&PrimaryStreamPosition::Live {
                    cursor: cursor.clone()
                })
                .is_err()
        );
        tracker
            .observe(&PrimaryStreamPosition::Snapshot {
                cursor: cursor.clone(),
                replay_events: 0,
            })
            .unwrap();
        cursor.sequence = 11;
        tracker
            .observe(&PrimaryStreamPosition::Live {
                cursor: cursor.clone(),
            })
            .unwrap();
        assert!(
            tracker
                .observe(&PrimaryStreamPosition::Live {
                    cursor: cursor.clone()
                })
                .is_err()
        );
        cursor.stream_id = "replacement".into();
        cursor.sequence = 12;
        assert!(
            tracker
                .observe(&PrimaryStreamPosition::Live { cursor })
                .is_err()
        );
    }
    #[test]
    fn cursor_frames_retain_legacy_event_decoding_and_complete_text() {
        let text = "synthetic λ\n".repeat(10000);
        let event = ServerEvent::TextDelta { text: text.clone() };
        let position = PrimaryStreamPosition::Live {
            cursor: PrimaryStreamCursor {
                session_id: "session".into(),
                stream_id: "stream".into(),
                sequence: 7,
                request_id: 1,
                origin: None,
            },
        };
        let json = encode_primary_event(&event, &position).unwrap();
        assert!(
            matches!(serde_json::from_str::<ServerEvent>(&json).unwrap(),ServerEvent::TextDelta{text:actual} if actual==text)
        );
        let raw: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            serde_json::from_value::<PrimaryStreamPosition>(raw["primary_stream"].clone()).unwrap(),
            position
        );
    }
}
