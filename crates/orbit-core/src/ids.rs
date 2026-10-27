use std::num::NonZeroU64;

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(NonZeroU64);
    };
}

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
