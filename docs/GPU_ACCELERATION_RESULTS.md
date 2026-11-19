# DRY OS — Project Ascension 7.0: GPU Acceleration & VirtIO-GPU 3D (`Virgl`) Results

**Milestone:** E (`Gerçek Grafik Hızlandırmaya Yaklaşma`)  
**Status Summary:**  
- **VirtIO-GPU 3D Protocol & QEMU/Virgl Smoke Path:** `VERIFIED_QEMU` (over isolated `QEMU virtio-gpu-gl-pci` + `egl-headless` Mesa software `llvmpipe` — **not** physical GPU silicon execution)  
- **Full Mesa OpenGL / Vulkan (`Venus`) 3D Userspace Stack:** `PARTIAL` / `UNSUPPORTED` (DRM/Virgl protocol verified; full Mesa libc/gallium port or MicroVM GL guest stack deferred)

> [!IMPORTANT]
> Passing `GET_CAPSET_INFO` alone does not constitute 3D acceleration, and passing QEMU `virglrenderer` with Mesa software `llvmpipe` (`LIBGL_ALWAYS_SOFTWARE=1`) is strictly `VERIFIED_QEMU` — **not** physical Intel Iris Xe or NVIDIA RTX 3050 Ti 3D hardware acceleration.

---

## 1. Isolated Host Environment & Dependency Discovery

In Ascension 6.0, QEMU VirtIO-GPU 3D probing reported `HOST_VIRGL_UNAVAILABLE` because `qemu-hw-display-virtio-gpu-gl`, `qemu-hw-display-virtio-gpu-pci-gl`, `qemu-ui-opengl`, `qemu-ui-egl-headless`, and `virglrenderer` were not installed globally on the host.

Consistent with Ascension 7.0's strict rule against mutating host system packages (`pacman -S` is forbidden), `tools/bootstrap_qemu_virgl_env.py` downloads and extracts ONLY the required shared objects into the gitignored `.toolchains/virgl-env/` sandbox:

- **Packages extracted (`pacman -Sp` read-only URL resolution):**
  - `virglrenderer-1.3.0-2-x86_64.pkg.tar.zst` -> `.toolchains/virgl-env/lib/libvirglrenderer.so.1.11.0` (`725,288 B`)
  - `qemu-hw-display-virtio-gpu-gl-11.1.3-1-x86_64.pkg.tar.zst` -> `.toolchains/virgl-env/qemu-modules/hw-display-virtio-gpu-gl.so` (`47,248 B`)
  - `qemu-hw-display-virtio-gpu-pci-gl-11.1.3-1-x86_64.pkg.tar.zst` -> `.toolchains/virgl-env/qemu-modules/hw-display-virtio-gpu-pci-gl.so` (`14,136 B`)
  - `qemu-ui-opengl-11.1.3-1-x86_64.pkg.tar.zst` -> `.toolchains/virgl-env/qemu-modules/ui-opengl.so` (`39,520 B`)
  - `qemu-ui-egl-headless-11.1.3-1-x86_64.pkg.tar.zst` -> `.toolchains/virgl-env/qemu-modules/ui-egl-headless.so` (`14,344 B`)
- **Host GPU Safety Isolation:** On the host system, `/usr/share/glvnd/egl_vendor.d/10_nvidia.json` caused headless QEMU teardown to probe NVIDIA EGL and abort (`SIGABRT` `corrupted size vs. prev_size`). `tools/bootstrap_qemu_virgl_env.py` and `tools/probe_virtio_gpu_3d.py` explicitly set `__EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/50_mesa.json` and `LIBGL_ALWAYS_SOFTWARE=1`, guaranteeing pure CPU-side Mesa `llvmpipe` execution with zero interaction with physical `8086:9a49` or `10de:25a0`.

**Reproducible verification commands:**
```bash
python3 tools/bootstrap_qemu_virgl_env.py
python3 tools/probe_virtio_gpu_3d.py --live-qemu
```

---

## 2. Live QEMU VirtIO-GPU 3D Protocol & Pixel Readback Verification

Implemented in [userspace/src/virtio_gpu_3d.rs](file:///home/zwannfrederick/Masa%C3%BCst%C3%BC/Sektor/Coding/dryos/userspace/src/virtio_gpu_3d.rs) and wired into [userspace/src/virtio_gpu.rs](file:///home/zwannfrederick/Masa%C3%BCst%C3%BC/Sektor/Coding/dryos/userspace/src/virtio_gpu.rs) and [userspace/src/bin/dry_gpu.rs](file:///home/zwannfrederick/Masa%C3%BCst%C3%BC/Sektor/Coding/dryos/userspace/src/bin/dry_gpu.rs):

| Step | VirtIO / Virgl Wire Sequence | Observed Result (`results_virtio_gpu_3d_live.json`) |
| :--- | :--- | :--- |
| 1. Feature & Capset Query | `VIRTIO_GPU_F_VIRGL` (`bit 0`), `GET_CAPSET_INFO` | `virgl=1 capsets=2 id=1 max_ver=1 max_sz=308` |
| 2. Unit Invariant Check | `verify_virtio_3d_protocol_invariants()` (`size_of` asserts + 84 B Virgl packet check) | `unit_ok=1` |
| 3. 3D Context Create | `VIRTIO_GPU_CMD_CTX_CREATE` (`0x0200`, `ctx_id=1`) | `ctx=1` (`RESP_OK_NODATA`) |
| 4. 3D Texture + Backing + Bind | `RESOURCE_CREATE_3D` (`0x0204`, `16x16` `B8G8R8A8_UNORM`) + `RESOURCE_ATTACH_BACKING` (`DEVICE_DMA` 4 KiB page) + `CTX_ATTACH_RESOURCE` (`0x0202`) | `res=1` (`RESP_OK_NODATA`) |
| 5. Fenced 3D Clear Execution | `SUBMIT_3D` (`0x0207`, `VIRTIO_GPU_FLAG_FENCE`, `fence_id=1`): `CREATE_OBJECT(SURFACE=8)` -> `SET_FRAMEBUFFER_STATE` -> `CLEAR(COLOR0, [1.0, 0.0, 0.0, 1.0])` -> `DESTROY_OBJECT(SURFACE=8)` | `submit=1` (`RESP_OK_NODATA`) |
| 6. Host->Guest Pixel Readback | Zero-initialized DMA buffer -> `TRANSFER_FROM_HOST_3D` (`0x0206`, `ctx_id=1`, `stride=64`, `VIRTIO_GPU_FLAG_FENCE`) | `xfer=0x1100`, `px=0xffff0000`, `match=1` (exact little-endian `B8G8R8A8` red!) |
| 7. Negative Malformed Command | Truncated `SUBMIT_3D` packet (`size=1024` inside `32 B` descriptor) | `neg_err=0x1205` (`RESP_ERR_INVALID_PARAMETER`) |
| 8. Teardown & Leak Audit | `CTX_DETACH_RESOURCE` + `RESOURCE_UNREF` + `CTX_DESTROY` + `VSPACE_MAP_OP_UNMAP_4K` + `sys_cap_revoke` | `clean=1`, `leaked_frames=0`, QEMU `exit=33` (`16.295s`) |

All pre-existing 2D VirtIO-GPU and Ascension 6.0 suites remain green post-edit (`--p16-only` **4/4 PASS**, `--gpu-only` **1/1 PASS** with `leaked_frames=0`).

---

## 3. Virgl vs. Venus & Mesa Userspace Feasibility

| Dimension | **Virgl** (`capset_id = 1 / 2`) | **Venus** (Vulkan over `virtio-gpu` `capset_id = 4`) |
| :--- | :--- | :--- |
| Host & QEMU Requirements | `virglrenderer` + OpenGL/EGL (`egl-headless`); works with standard `attach_backing` (`blob=off`) | Requires `virtio-gpu-gl-pci,blob=on,hostmem=...`, `CAPSET_VENUS`, and host Vulkan ICD (`vk_*`) |
| DRY OS Kernel Changes Needed | **None.** Works over existing `SYS_DEV_MAP` + `SYS_DMA_ALLOC` + MSI-X `Notification` (`ABI v0.2`) | Would need `virtio-gpu` PCI BAR host-visible coherent blob aperture mapping (`mmap`-like memslot window) |
| Guest Command Complexity | Medium: Gallium pipe objects + TGSI shaders (~100 C/Rust opcodes); minimal `no_std` smoke paths fit directly | High: Full Vulkan command-stream serialization ring (`vn_cs`) requiring Mesa `libvulkan_virtio` |
| Verdict for DRY OS | **`RECOMMENDED` for immediate 3D QEMU CI & compute/blit bring-up** (`VERIFIED_QEMU`) | `DEFERRED` until Linux GPU MicroVM guest (`Mesa` + Linux `virtio-gpu.ko`) is enabled |

### Mesa Userspace Compatibility Assessment
DRY OS native userland is strictly `#![no_std]` Rust (`dry-userspace` + native GUI apps), without POSIX `dlopen`, glibc, or LLVM `libclc`. Compiling full Mesa Gallium `virgl_dri.so` or `vulkan_virtio` natively inside DRY OS Ring-3 would pull in C++17, LLVM, libdrm, POSIX threads, and dynamic linking — contradicting `Minimum Overhead`.

Therefore, DRY OS preserves two realistic, complementary acceleration tracks:
1. **Native Minimal Path (`no_std`):** Ring-3 `dry-gpu` exposes Virgl/GPU-Service fence & surface primitives (`SurfaceBufferSet` + `FenceTimeline` + `VIRTIO_GPU_FLAG_FENCE`), suitable for lightweight 2D/3D shader blits, compositor cursors, and deterministic hardware tests without C++/Mesa bloat.
2. **Linux GPU MicroVM Path (Recommended for heavy 3D/OpenGL/Vulkan):** Run unmodified upstream Mesa + Linux DRM inside an isolated guest VM communicating rendered buffers/fences back to `dry-gpu-svc` / `dry-display`.

---

## 4. Integration with `dry-gpu-svc` & Remaining Acceleration Gaps

- **Buffer & Fence Integration:** `VirtioGpu2d` already enforces `VIRTIO_GPU_FLAG_FENCE` (`hdr.flags = 1`) on `SUBMIT_3D` and `TRANSFER_FROM_HOST_3D` alongside `IRQF_ONESHOT` MSI-X wakeups (`submit_sync`), while `dry-gpu-svc` tracks monotonic GPU timeline fences (`FenceTimeline`) and `SurfaceBufferSet` bounds independently of vendor hardware.
- **Explicit Gaps Remaining:**
  1. Full triangle rasterization / GLSL shader pipeline (only surface clear + pixel readback verified so far).
  2. Multi-process 3D surface sharing between client apps and `dry-gpu` over shared `SurfaceBufferSet` handles.
  3. Native Mesa OpenGL / Vulkan ICD inside DRY OS Ring-3 (`UNSUPPORTED`, planned via Linux MicroVM guest).
  4. Physical Intel Iris Xe / NVIDIA GA107 3D hardware execution (`UNSUPPORTED` / awaiting bare-metal driver bring-up).
