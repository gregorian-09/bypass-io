#include <stdbool.h>
#include <stddef.h>

#include <spdk/env.h>
#include <spdk/nvme.h>
#include <spdk/nvme_spec.h>

int bypass_spdk_env_init(const char *name) {
    struct spdk_env_opts opts;
    spdk_env_opts_init(&opts);
    opts.name = name;
    return spdk_env_init(&opts);
}

void bypass_spdk_env_fini(void) {
    spdk_env_fini();
}

bool bypass_spdk_cpl_is_error(const struct spdk_nvme_cpl *completion) {
    return spdk_nvme_cpl_is_error(completion);
}

const char *bypass_spdk_cpl_status_string(const struct spdk_nvme_cpl *completion) {
    return spdk_nvme_cpl_get_status_string(&completion->status);
}
