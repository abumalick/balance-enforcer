//! Library crate backing the `balance-enforcer` binary. Split out so that
//! integration tests in `tests/` can reach the pure logic modules.

pub mod audio;
pub mod config;
pub mod enforcer;
pub mod install;
pub mod logging;
pub mod windows_impl;
