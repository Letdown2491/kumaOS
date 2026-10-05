# vendored vgem sources

Out-of-tree build sources for the vgem DRM driver, copied verbatim from
upstream: `torvalds/linux` tag `v6.17`, `drivers/gpu/drm/vgem/`
(`vgem_drv.c`, `vgem_drv.h`, `vgem_fence.c`). License: GPL-2.0 — see the
headers in each file.

## Why vendored

The kvm action builds vgem when the runner has no DRM render node (the
20261004 runner image's azure kernel dropped vgem from modules-extra and
the guest's virgl GL cannot initialize without a node). The build
previously fetched these files from `raw.githubusercontent.com` with
anonymous curl, which GitHub rate-limits (HTTP 429) from Actions egress
IPs — the 2026-10-05 `iso` lap died on exactly that. Vendored sources
make the action hermetic: no network fetch, no 429 lottery.

## Upstream policy

Re-vendor by hand when the driver changes upstream in a way that
matters (it has been stable for years). The `Makefile` is NOT vendored:
the action writes its own two-line out-of-tree Makefile.
