//! Typed document configuration shared by native Rust and the WIT component.
//!
//! Configuration is host input, not a persisted CRDT artifact. It therefore
//! travels as WIT records instead of maintaining a second binary protocol.

use crate::error::EngineError;
use crate::limits::ResourceLimits;
use crate::replica::ReplicaConfig;

/// Everything a document constructor accepts, as one typed value.
#[derive(Clone, Debug, Default)]
pub struct DocumentConfig {
    pub replica: ReplicaConfig,
    pub limits: ResourceLimits,
}

impl DocumentConfig {
    pub fn validate(&self) -> Result<(), EngineError> {
        self.replica.validate()?;
        self.limits.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::allocator::{AdaptiveDmaxConfig, DMAX_HARD_CEILING};

    #[test]
    fn defaults_are_coherent() {
        DocumentConfig::default().validate().expect("defaults");
    }

    #[test]
    fn rejects_static_and_adaptive_dmax_above_the_hard_ceiling() {
        let mut config = DocumentConfig::default();
        config.replica.dmax = DMAX_HARD_CEILING + 1;
        assert!(config.validate().is_err());

        let mut config = DocumentConfig::default();
        config.replica.adaptive_dmax = Some(AdaptiveDmaxConfig {
            floor: config.replica.dmax,
            ceiling: DMAX_HARD_CEILING + 1,
            ..AdaptiveDmaxConfig::default()
        });
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_state_that_cannot_be_checkpointed() {
        let mut config = DocumentConfig::default();
        config.limits.max_snapshot_items = config.limits.max_document_units - 1;
        assert!(config.validate().is_err());
    }
}
