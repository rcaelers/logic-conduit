//! Payload-type negotiation for binary-signal ports
//!
//! [`SampleKind`] names the concrete Rust payload a port can carry for a
//! raw binary signal — mirrors [`super::protocol::ProtocolKind`]'s shape,
//! but negotiates the connection's *type* rather than its transport.
//! Every wiring path ([`super::pipeline::Pipeline::connect_with_buffer`],
//! [`super::manager::PipelineManager::add_node_deferred`] and
//! `restart_node`) shares [`negotiate`] so there is exactly one place
//! this logic lives.

use crate::runtime::sample::{Sample, SampleBlock};
use std::any::TypeId;

/// Which concrete payload a binary-signal port can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SampleKind {
    /// One RLE edge at a time (`Sample`).
    Edge,
    /// A densely packed-bit block (`SampleBlock`).
    Block,
}

impl SampleKind {
    /// The concrete Rust payload type this kind names.
    ///
    /// Deliberately **not** called `type_id`: `std::any::Any::type_id`
    /// (`&self -> TypeId`) is in scope wherever this crate does its usual
    /// `Box<dyn Any>` type erasure, and calling `kind.type_id()` on a
    /// *borrowed* `&SampleKind` there silently resolves to `Any`'s method
    /// instead of this one (method resolution finds the exact `&Self`
    /// receiver match before it deref's down to try inherent methods on
    /// `SampleKind` itself) — returning `TypeId::of::<SampleKind>()`, the
    /// same value for every variant, no compiler error. Learned the hard
    /// way; keep the distinct name so `kind.type_id()` simply doesn't
    /// compile as "this method" by accident.
    pub fn payload_type(self) -> TypeId {
        match self {
            SampleKind::Edge => TypeId::of::<Sample>(),
            SampleKind::Block => TypeId::of::<SampleBlock>(),
        }
    }
}

/// Negotiates one connection's payload type.
///
/// `offered`/`accepted` are the producer's/consumer's declared kind
/// lists, most preferred first; empty means "not polymorphic — only this
/// port's own declared type applies" (every existing node's default via
/// [`super::node::ProcessNode::output_sample_kinds`]/`input_sample_kinds`).
/// Empty on both sides falls back to a plain equality check between
/// `from_type`/`to_type` (today's behavior, unchanged); otherwise
/// intersects the two kind sets, producer preference order winning ties.
/// Returns `None` if there's no common kind.
pub fn negotiate(
    offered: &[SampleKind],
    from_type: TypeId,
    accepted: &[SampleKind],
    to_type: TypeId,
) -> Option<TypeId> {
    if offered.is_empty() && accepted.is_empty() {
        return (from_type == to_type).then_some(from_type);
    }

    let offered_kinds: Vec<SampleKind> = if offered.is_empty() {
        vec![kind_of(from_type)?]
    } else {
        offered.to_vec()
    };
    let accepted_kinds: Vec<SampleKind> = if accepted.is_empty() {
        vec![kind_of(to_type)?]
    } else {
        accepted.to_vec()
    };

    offered_kinds
        .into_iter()
        .find(|kind| accepted_kinds.contains(kind))
        .map(SampleKind::payload_type)
}

/// Maps a concrete `Sample`/`SampleBlock` `TypeId` back to its `SampleKind`,
/// for the "one side didn't declare a kind list" fallback above. Returns
/// `None` for any other type — a port declaring no kind list but backed
/// by neither `Sample` nor `SampleBlock` simply never enters negotiation
/// (its `offered`/`accepted` are always empty too, so `negotiate` never
/// reaches this branch for it).
fn kind_of(type_id: TypeId) -> Option<SampleKind> {
    if type_id == TypeId::of::<Sample>() {
        Some(SampleKind::Edge)
    } else if type_id == TypeId::of::<SampleBlock>() {
        Some(SampleKind::Block)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_empty_falls_back_to_equality() {
        let sample = TypeId::of::<Sample>();
        let block = TypeId::of::<SampleBlock>();
        assert_eq!(negotiate(&[], sample, &[], sample), Some(sample));
        assert_eq!(negotiate(&[], sample, &[], block), None);
    }

    #[test]
    fn producer_preference_order_wins() {
        let sample = TypeId::of::<Sample>();
        let block = TypeId::of::<SampleBlock>();
        // Producer prefers Block, consumer accepts both -> Block wins.
        assert_eq!(
            negotiate(
                &[SampleKind::Block, SampleKind::Edge],
                block,
                &[SampleKind::Edge, SampleKind::Block],
                sample,
            ),
            Some(block)
        );
        // Producer only offers Edge, consumer accepts both -> Edge wins.
        assert_eq!(
            negotiate(
                &[SampleKind::Edge],
                sample,
                &[SampleKind::Edge, SampleKind::Block],
                sample,
            ),
            Some(sample)
        );
    }

    #[test]
    fn no_common_kind_is_none() {
        let sample = TypeId::of::<Sample>();
        let block = TypeId::of::<SampleBlock>();
        assert_eq!(
            negotiate(&[SampleKind::Block], block, &[SampleKind::Edge], sample),
            None
        );
    }

    #[test]
    fn one_sided_polymorphism_matches_the_others_fixed_type() {
        let sample = TypeId::of::<Sample>();
        let block = TypeId::of::<SampleBlock>();
        // Producer offers both; consumer is a plain fixed-Sample port.
        assert_eq!(
            negotiate(&[SampleKind::Block, SampleKind::Edge], block, &[], sample),
            Some(sample)
        );
        // Producer is fixed-Block; consumer accepts both.
        assert_eq!(
            negotiate(&[], block, &[SampleKind::Edge, SampleKind::Block], sample),
            Some(block)
        );
    }
}
