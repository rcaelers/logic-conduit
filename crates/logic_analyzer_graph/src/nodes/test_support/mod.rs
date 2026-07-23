//! Private fixtures for isolated concrete-node tests.

mod assertion;
mod endpoints;
mod lookup;

pub(crate) use assertion::{
    assert_node_registration_isolated, assert_node_registration_isolated_with_state,
};
pub(crate) use lookup::{default_node_state, node_builder, node_name};
