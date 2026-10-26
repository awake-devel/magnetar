//! Every integration test of `magnetar-ffi` in one binary (D78): one module per former
//! `tests/*.rs` file, so the workspace links one test executable per crate instead of one per
//! file. Run one module with `cargo test -p magnetar-ffi --test it <module>::`.

mod common;

mod filters;
mod liveness;
mod magnetar;
mod media;
mod stats;
mod store;
