//! Persistent identities are distinct types, serialized as their existing integer values.
use derive_more::{Display, From, FromStr};
use serde::{Deserialize, Serialize};

macro_rules! identity {
    ($($name:ident),+ $(,)?) => { $(
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, From, Display, FromStr)]
        #[serde(transparent)]
        pub(crate) struct $name(pub(crate) i64);
    )+ };
}
identity!(
    UserId,
    ReminderId,
    DraftId,
    NaggingId,
    CompletionId,
    ConfirmationId,
    VoiceJobId
);
