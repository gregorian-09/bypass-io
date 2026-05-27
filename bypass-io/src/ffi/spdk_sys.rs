//! Minimal SPDK C type declarations.
//!
//! This module intentionally declares only opaque handle types and callback
//! shapes. It does not link SPDK functions yet, which keeps `--all-features`
//! builds usable on machines without SPDK installed.

use std::ffi::c_void;
#[cfg(bypass_io_native_spdk)]
use std::os::raw::c_char;
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

/// Opaque `struct spdk_nvme_transport_id`.
#[repr(C)]
pub struct spdk_nvme_transport_id {
    _private: [u8; 0],
}

/// Opaque `struct spdk_nvme_probe_ctx`.
#[repr(C)]
pub struct spdk_nvme_probe_ctx {
    _private: [u8; 0],
}

/// Opaque `struct spdk_nvme_ctrlr_opts`.
#[repr(C)]
pub struct spdk_nvme_ctrlr_opts {
    _private: [u8; 0],
}

/// Callback shape used by SPDK NVMe I/O completion functions.
pub type SpdkNvmeIoCompletionCb =
    unsafe extern "C" fn(ctx: *mut c_void, completion: *const spdk_nvme_cpl);

/// SPDK probe callback used to decide whether to attach a controller.
pub type SpdkNvmeProbeCb = unsafe extern "C" fn(
    cb_ctx: *mut c_void,
    trid: *const spdk_nvme_transport_id,
    opts: *mut spdk_nvme_ctrlr_opts,
) -> bool;

/// SPDK attach callback called after a controller is attached.
pub type SpdkNvmeAttachCb = unsafe extern "C" fn(
    cb_ctx: *mut c_void,
    trid: *const spdk_nvme_transport_id,
    ctrlr: *mut spdk_nvme_ctrlr,
    opts: *const spdk_nvme_ctrlr_opts,
);

/// Signed return code used by SPDK submission and polling functions.
pub type SpdkRc = c_int;

#[cfg(bypass_io_native_spdk)]
unsafe extern "C" {
    /// Probe NVMe controllers.
    pub fn spdk_nvme_probe(
        trid: *const spdk_nvme_transport_id,
        cb_ctx: *mut c_void,
        probe_cb: Option<SpdkNvmeProbeCb>,
        attach_cb: Option<SpdkNvmeAttachCb>,
        remove_cb: Option<unsafe extern "C" fn(*mut c_void, *mut spdk_nvme_ctrlr)>,
    ) -> SpdkRc;

    /// Return a controller's transport identifier.
    pub fn spdk_nvme_ctrlr_get_transport_id(
        ctrlr: *mut spdk_nvme_ctrlr,
    ) -> *const spdk_nvme_transport_id;

    /// Return a transport identifier as text.
    pub fn spdk_nvme_transport_id_trtype_str(trid: *const spdk_nvme_transport_id) -> *const c_char;

    /// Return the first active namespace for a controller.
    pub fn spdk_nvme_ctrlr_get_first_active_ns(ctrlr: *mut spdk_nvme_ctrlr) -> u32;

    /// Return the next active namespace after `prev_nsid`.
    pub fn spdk_nvme_ctrlr_get_next_active_ns(ctrlr: *mut spdk_nvme_ctrlr, prev_nsid: u32) -> u32;

    /// Return an active namespace handle.
    pub fn spdk_nvme_ctrlr_get_ns(ctrlr: *mut spdk_nvme_ctrlr, nsid: u32) -> *mut spdk_nvme_ns;

    /// Allocate an I/O queue pair.
    pub fn spdk_nvme_ctrlr_alloc_io_qpair(
        ctrlr: *mut spdk_nvme_ctrlr,
        opts: *const c_void,
        opts_size: usize,
    ) -> *mut spdk_nvme_qpair;

    /// Return the namespace sector size.
    pub fn spdk_nvme_ns_get_sector_size(ns: *const spdk_nvme_ns) -> u32;

    /// Return the namespace capacity in sectors.
    pub fn spdk_nvme_ns_get_num_sectors(ns: *const spdk_nvme_ns) -> u64;

    /// Submit an NVMe namespace read.
    pub fn spdk_nvme_ns_cmd_read(
        ns: *mut spdk_nvme_ns,
        qpair: *mut spdk_nvme_qpair,
        payload: *mut c_void,
        lba: u64,
        lba_count: u32,
        cb_fn: Option<SpdkNvmeIoCompletionCb>,
        cb_arg: *mut c_void,
        io_flags: u32,
    ) -> SpdkRc;

    /// Submit an NVMe namespace write.
    pub fn spdk_nvme_ns_cmd_write(
        ns: *mut spdk_nvme_ns,
        qpair: *mut spdk_nvme_qpair,
        payload: *mut c_void,
        lba: u64,
        lba_count: u32,
        cb_fn: Option<SpdkNvmeIoCompletionCb>,
        cb_arg: *mut c_void,
        io_flags: u32,
    ) -> SpdkRc;

    /// Submit an NVMe namespace flush.
    pub fn spdk_nvme_ns_cmd_flush(
        ns: *mut spdk_nvme_ns,
        qpair: *mut spdk_nvme_qpair,
        cb_fn: Option<SpdkNvmeIoCompletionCb>,
        cb_arg: *mut c_void,
    ) -> SpdkRc;

    /// Process completions for a queue pair.
    pub fn spdk_nvme_qpair_process_completions(
        qpair: *mut spdk_nvme_qpair,
        max_completions: u32,
    ) -> SpdkRc;
}
