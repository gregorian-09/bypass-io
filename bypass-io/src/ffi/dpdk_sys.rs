//! Minimal DPDK C type declarations.
//!
//! This module declares opaque DPDK handles used by the safe Phase 3 backend
//! boundary. It intentionally does not link DPDK functions yet, so
//! `--all-features` builds work on machines without DPDK installed.

use std::os::raw::{c_char, c_int};

/// Opaque `struct rte_mempool`.
#[repr(C)]
pub struct rte_mempool {
    _private: [u8; 0],
}

/// Opaque `struct rte_mbuf`.
#[repr(C)]
pub struct rte_mbuf {
    _private: [u8; 0],
}

/// Opaque `struct rte_flow`.
#[repr(C)]
pub struct rte_flow {
    _private: [u8; 0],
}

/// Opaque `struct rte_flow_error`.
#[repr(C)]
pub struct rte_flow_error {
    _private: [u8; 0],
}

/// Return code used by many DPDK C APIs.
pub type RteRc = c_int;

/// C string pointer used by EAL argument arrays.
pub type RteArgv = *mut *mut c_char;
