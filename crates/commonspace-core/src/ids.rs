//! Stable string identifiers. UUIDv4 under the hood, newtyped so the compiler
//! prevents mixing a `TaskId` into a `ConversationId` slot.

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident, $prefix:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            /// Generate a new random id with a readable prefix.
            pub fn generate() -> Self {
                Self(format!("{}_{}", $prefix, uuid::Uuid::new_v4().simple()))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

id_type!(
    #[doc = "A workspace: one or more authorized folder roots."]
    WorkspaceId,
    "ws"
);
id_type!(
    #[doc = "A conversation thread."]
    ConversationId,
    "conv"
);
id_type!(
    #[doc = "A single chat message."]
    MessageId,
    "msg"
);
id_type!(
    #[doc = "A task (one delegated unit of work)."]
    TaskId,
    "task"
);
id_type!(
    #[doc = "A provider-side session (maps to CLI session/thread ids)."]
    SessionId,
    "sess"
);
id_type!(
    #[doc = "A tool invocation within a session."]
    ToolCallId,
    "tool"
);
id_type!(
    #[doc = "A permission request awaiting or holding a decision."]
    PermissionRequestId,
    "perm"
);
id_type!(
    #[doc = "A generated or modified artifact."]
    ArtifactId,
    "art"
);
id_type!(
    #[doc = "A journaled file operation (unit of undo)."]
    FileOperationId,
    "fop"
);

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_prefixed_and_unique() {
        let a = TaskId::generate();
        let b = TaskId::generate();
        assert!(a.0.starts_with("task_"));
        assert_ne!(a, b);
    }

    #[test]
    fn ids_serialize_transparently() {
        let id = WorkspaceId("ws_abc".into());
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"ws_abc\"");
    }
}
