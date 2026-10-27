use std::num::NonZeroU64;

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident) => {
        /// A unique identifier for a specific type of object.
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(NonZeroU64);

        /// Returns the raw `u64` value of the identifier.
        impl From<NonZeroU64> for $name {
            fn from(raw: NonZeroU64) -> Self {
                Self(raw)
            }
        }
    };
}

id_type!(
    /// A window in the system.
    WindowId
);

id_type!(
    /// A monitor output while it is connected. A monitor that is
    /// unplugged and replugged will have a different `OutputId`.
    OutputId
);

id_type!(
    /// A workspace on some monitor output. A workspace that is moved
    /// to a different monitor output will have a different `WorkspaceId`.
    WorkspaceId
);

/// Recognizes a monitor across unplugging and replugging.
///
/// This is not an identifier because even though two monitors never
/// share an `OutputKey` at the same time, this is not unique over time.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OutputKey(pub String);

/// A simple allocator for unique IDs of type `T`.
#[derive(Debug, Default)]
pub struct IdAllocator {
    /// The last allocated ID. Starts at 0, so the first allocated ID will be 1.
    last: u64,
}

impl IdAllocator {
    /// Allocates a new unique ID of type `T`.
    pub fn allocate<T: From<NonZeroU64>>(&mut self) -> T {
        self.last += 1;
        T::from(NonZeroU64::new(self.last).unwrap_or(NonZeroU64::MIN))
    }
}
