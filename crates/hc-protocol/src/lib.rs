//! Shared protocol types and traits for Honeycomb.

pub mod protocol {
    //! Stable schemas shared across runtime, storage, and UI layers.

    /// Marker trait for records that carry a stable identifier.
    pub trait RecordId {
        fn id(&self) -> &str;
    }
}

