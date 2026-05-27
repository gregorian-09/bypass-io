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

/// Opaque `struct rte_eth_conf`.
#[repr(C)]
pub struct rte_eth_conf {
    _private: [u8; 0],
}

/// Opaque `struct rte_eth_rxconf`.
#[repr(C)]
pub struct rte_eth_rxconf {
    _private: [u8; 0],
}

/// Opaque `struct rte_eth_txconf`.
#[repr(C)]
pub struct rte_eth_txconf {
    _private: [u8; 0],
}

/// Opaque `struct rte_flow_attr`.
#[repr(C)]
pub struct rte_flow_attr {
    _private: [u8; 0],
}

/// Opaque `struct rte_flow_item`.
#[repr(C)]
pub struct rte_flow_item {
    _private: [u8; 0],
}

/// Opaque `struct rte_flow_action`.
#[repr(C)]
pub struct rte_flow_action {
    _private: [u8; 0],
}

/// Return code used by many DPDK C APIs.
pub type RteRc = c_int;

/// C string pointer used by EAL argument arrays.
pub type RteArgv = *mut *mut c_char;

#[cfg(bypass_io_native_dpdk)]
unsafe extern "C" {
    /// Initialize DPDK EAL.
    pub fn rte_eal_init(argc: c_int, argv: RteArgv) -> RteRc;

    /// Create an mbuf pool for packet buffers.
    pub fn rte_pktmbuf_pool_create(
        name: *const c_char,
        n: u32,
        cache_size: u32,
        priv_size: u16,
        data_room_size: u16,
        socket_id: c_int,
    ) -> *mut rte_mempool;

    /// Configure an Ethernet device.
    pub fn rte_eth_dev_configure(
        port_id: u16,
        nb_rx_queue: u16,
        nb_tx_queue: u16,
        eth_conf: *const rte_eth_conf,
    ) -> RteRc;

    /// Set up an RX queue.
    pub fn rte_eth_rx_queue_setup(
        port_id: u16,
        rx_queue_id: u16,
        nb_rx_desc: u16,
        socket_id: c_int,
        rx_conf: *const rte_eth_rxconf,
        mb_pool: *mut rte_mempool,
    ) -> RteRc;

    /// Set up a TX queue.
    pub fn rte_eth_tx_queue_setup(
        port_id: u16,
        tx_queue_id: u16,
        nb_tx_desc: u16,
        socket_id: c_int,
        tx_conf: *const rte_eth_txconf,
    ) -> RteRc;

    /// Start an Ethernet device.
    pub fn rte_eth_dev_start(port_id: u16) -> RteRc;

    /// Create a flow rule.
    pub fn rte_flow_create(
        port_id: u16,
        attr: *const rte_flow_attr,
        pattern: *const rte_flow_item,
        actions: *const rte_flow_action,
        error: *mut rte_flow_error,
    ) -> *mut rte_flow;
}

// DPDK's packet burst and mbuf data-access APIs are header-inline in common
// releases. The native adapter must use generated bindings or a small C shim
// for those calls instead of assuming a linkable shared-library symbol exists.
