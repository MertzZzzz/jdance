// SPDX-License-Identifier: GPL-3.0-or-later
//! Stable object IDs.
//!
//! IDs are allocated from a per-project monotonic counter and are never
//! reused, so undo/redo and persistence can refer to objects by ID.

use serde::{Deserialize, Serialize};

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub u64);
    };
}

id_type!(
    /// Channel Rack channel.
    ChannelId
);
id_type!(
    /// Pattern.
    PatternId
);
id_type!(
    /// Mixer track.
    TrackId
);

impl TrackId {
    /// The master track always has ID 0; allocated IDs start at 1.
    pub const MASTER: TrackId = TrackId(0);
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdAllocator {
    next: u64,
}

impl Default for IdAllocator {
    fn default() -> Self {
        Self { next: 1 }
    }
}

impl IdAllocator {
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> u64 {
        let id = self.next;
        self.next += 1;
        id
    }
}
