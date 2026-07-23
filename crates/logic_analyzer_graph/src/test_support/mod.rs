//! Test-only adapters for driving graph contracts without exposing processing types.

mod adapters;

pub use adapters::{
    TestBufferedFakeConfig, TestBufferedFakeController, TestBufferedFakeProvider,
    TestDeterministicFakeConfig, TestDeterministicFakeController, TestDeterministicFakeProvider,
};
