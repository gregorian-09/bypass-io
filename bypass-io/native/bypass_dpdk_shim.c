#include <sys/types.h>
#include <stdint.h>
#include <string.h>

#include <rte_ethdev.h>
#include <rte_mbuf.h>

int bypass_dpdk_configure_port(
    uint16_t port_id,
    uint16_t rx_queues,
    uint16_t tx_queues,
    uint16_t rx_desc,
    uint16_t tx_desc,
    struct rte_mempool *pool,
    int socket_id,
    int promiscuous
) {
    struct rte_eth_conf port_conf;
    memset(&port_conf, 0, sizeof(port_conf));

    int rc = rte_eth_dev_configure(port_id, rx_queues, tx_queues, &port_conf);
    if (rc < 0) {
        return rc;
    }

    for (uint16_t queue = 0; queue < rx_queues; queue++) {
        rc = rte_eth_rx_queue_setup(port_id, queue, rx_desc, socket_id, NULL, pool);
        if (rc < 0) {
            return rc;
        }
    }

    for (uint16_t queue = 0; queue < tx_queues; queue++) {
        rc = rte_eth_tx_queue_setup(port_id, queue, tx_desc, socket_id, NULL);
        if (rc < 0) {
            return rc;
        }
    }

    rc = rte_eth_dev_start(port_id);
    if (rc < 0) {
        return rc;
    }

    if (promiscuous) {
        rte_eth_promiscuous_enable(port_id);
    }

    return 0;
}

uint16_t bypass_dpdk_rx_burst(
    uint16_t port_id,
    uint16_t queue_id,
    struct rte_mbuf **rx_pkts,
    uint16_t nb_pkts
) {
    return rte_eth_rx_burst(port_id, queue_id, rx_pkts, nb_pkts);
}

uint16_t bypass_dpdk_tx_burst(
    uint16_t port_id,
    uint16_t queue_id,
    struct rte_mbuf **tx_pkts,
    uint16_t nb_pkts
) {
    return rte_eth_tx_burst(port_id, queue_id, tx_pkts, nb_pkts);
}

struct rte_mbuf *bypass_dpdk_pktmbuf_alloc(struct rte_mempool *pool) {
    return rte_pktmbuf_alloc(pool);
}

void bypass_dpdk_pktmbuf_free(struct rte_mbuf *buf) {
    rte_pktmbuf_free(buf);
}

void *bypass_dpdk_pktmbuf_append(struct rte_mbuf *buf, uint16_t len) {
    return rte_pktmbuf_append(buf, len);
}

void *bypass_dpdk_pktmbuf_data(struct rte_mbuf *buf) {
    return rte_pktmbuf_mtod(buf, void *);
}

uint32_t bypass_dpdk_pktmbuf_pkt_len(struct rte_mbuf *buf) {
    return rte_pktmbuf_pkt_len(buf);
}
