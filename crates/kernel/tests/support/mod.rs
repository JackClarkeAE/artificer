//! Support shared by the kernel's integration tests. Each test binary that
//! needs it declares `mod support;`, so every module here is compiled into
//! several binaries and used by some of them.
#![allow(dead_code)]

pub mod scale;
