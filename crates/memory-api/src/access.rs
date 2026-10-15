// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The access context a host hands to memory.
//!
//! The host decides who the caller is and which spaces it may touch; memory
//! honours that as given and never widens it. Every store operation takes the
//! context, and a store applies it inside its own queries rather than
//! filtering what comes back.
//!
//! Swedish Embedded AB implements solutions for tenant-isolated agent memory
//! for its clients. If your team needs expertise in keeping one customer's
//! knowledge out of another customer's agent then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{MemoryError, SpaceId};

/// An operation beyond reading that the host may grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Permission {
    /// Add or replace sources in a writable space.
    Ingest,
    /// Append annotations to units in a writable space.
    Annotate,
    /// Delete sources from a writable space.
    Delete,
}

/// Upper bounds the host places on what one call may return.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    /// Most hits one retrieval returns, whatever the request asks for.
    pub max_results: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self { max_results: 50 }
    }
}

/// Who is asking, and what they may reach.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessContext {
    /// The caller's identity, recorded as the author of annotations.
    pub principal: String,
    /// Spaces the caller may read.
    pub read: BTreeSet<SpaceId>,
    /// Spaces the caller may write, subject to [`Self::permissions`].
    pub write: BTreeSet<SpaceId>,
    /// Operations granted on the writable spaces.
    pub permissions: BTreeSet<Permission>,
    /// Bounds on what one call returns.
    pub budget: Budget,
}

impl AccessContext {
    /// A context that can read nothing and write nothing.
    #[must_use]
    pub fn new(principal: impl Into<String>) -> Self {
        Self {
            principal: principal.into(),
            read: BTreeSet::new(),
            write: BTreeSet::new(),
            permissions: BTreeSet::new(),
            budget: Budget::default(),
        }
    }

    /// Allows reading `space`.
    #[must_use]
    pub fn reading(mut self, space: SpaceId) -> Self {
        self.read.insert(space);
        self
    }

    /// Allows reading `space` and exercising `permissions` in it.
    #[must_use]
    pub fn writing(mut self, space: SpaceId, permissions: &[Permission]) -> Self {
        self.read.insert(space.clone());
        self.write.insert(space);
        self.permissions.extend(permissions.iter().copied());
        self
    }

    /// Whether `space` is visible at all to this caller.
    #[must_use]
    pub fn can_read(&self, space: &SpaceId) -> bool {
        self.read.contains(space)
    }

    /// Checks that this caller may exercise `permission` in `space`.
    ///
    /// A space the caller cannot read is reported as [`MemoryError::NotFound`]
    /// so its existence is not revealed; a readable space the caller may not
    /// change is [`MemoryError::Forbidden`].
    ///
    /// # Errors
    /// See above.
    pub fn require(&self, space: &SpaceId, permission: Permission) -> Result<(), MemoryError> {
        if !self.can_read(space) {
            return Err(MemoryError::NotFound);
        }
        if self.write.contains(space) && self.permissions.contains(&permission) {
            Ok(())
        } else {
            Err(MemoryError::Forbidden(format!(
                "{permission:?} is not granted in space {space}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn space(id: &str) -> SpaceId {
        SpaceId::new(id)
    }

    #[test]
    fn an_unreadable_space_looks_absent_and_a_read_only_one_is_forbidden() {
        let ctx = AccessContext::new("p")
            .reading(space("ro"))
            .writing(space("rw"), &[Permission::Ingest]);
        assert!(matches!(
            ctx.require(&space("other"), Permission::Ingest),
            Err(MemoryError::NotFound)
        ));
        assert!(matches!(
            ctx.require(&space("ro"), Permission::Ingest),
            Err(MemoryError::Forbidden(_))
        ));
        assert!(matches!(
            ctx.require(&space("rw"), Permission::Delete),
            Err(MemoryError::Forbidden(_))
        ));
        assert!(ctx.require(&space("rw"), Permission::Ingest).is_ok());
    }
}
