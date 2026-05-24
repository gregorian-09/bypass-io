//! Minimal SPDK C type declarations.
//!
//! This module intentionally declares only opaque handle types and callback
//! shapes. It does not link SPDK functions yet, which keeps `--all-features`
//! builds usable on machines without SPDK installed.

use std::ffi::c_void;
use std::os::raw::c_int;

/// Opaque `struct spdk_nvme_ctrlr`.
#[repr(C)]
pub struct spdk_nvme_ctrlr {
    _private: [u8; 0],
}

/// Opaque `struct spdk_nvme_ns`.
#[repr(C)]
pub struct spdk_nvme_ns {
    _private: [u8; 0],
}

/// Opaque `struct spdk_nvme_qpair`.
#[repr(C)]
pub struct spdk_nvme_qpair {
    _private: [u8; 0],
}

/// Opaque `struct spdk_nvme_cpl`.
#[repr(C)]
pub struct spdk_nvme_cpl {
    _private: [u8; 0],
}

/// Callback shape used by SPDK NVMe I/O completion functions.
pub type SpdkNvmeIoCompletionCb =
    unsafe extern "C" fn(ctx: *mut c_void, completion: *const spdk_nvme_cpl);

/// Signed return code used by SPDK submission and polling functions.
pub type SpdkRc = c_int;
