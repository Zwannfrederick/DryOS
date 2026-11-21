// SPDX-License-Identifier: MIT OR GPL-2.0-only
/* Minimal static x86-64 Linux ELF64 workload for DRY OS Ascension 9.0 Linux-ABI proof. */

typedef unsigned char u8;
typedef unsigned int u32;
typedef unsigned long u64;
typedef long i64;

#define SYS_WRITE 1
#define SYS_CLOSE 3
#define SYS_BRK 12
#define SYS_GETPID 39
#define SYS_UNAME 63
#define SYS_ARCH_PRCTL 158
#define SYS_EXIT_GROUP 231
#define SYS_OPENAT 257

#define ARCH_SET_FS 0x1002
#define AT_FDCWD (-100L)
#define O_WRONLY 1
#define O_CREAT 64
#define O_TRUNC 512

struct utsname_min {
    char sysname[65];
    char nodename[65];
    char release[65];
    char version[65];
    char machine[65];
    char domainname[65];
};

static inline i64 sys1(u64 nr, u64 a1) {
    i64 ret;
    __asm__ volatile(
        "syscall"
        : "=a"(ret)
        : "a"(nr), "D"(a1)
        : "rcx", "r11", "memory");
    return ret;
}

static inline i64 sys2(u64 nr, u64 a1, u64 a2) {
    i64 ret;
    __asm__ volatile(
        "syscall"
        : "=a"(ret)
        : "a"(nr), "D"(a1), "S"(a2)
        : "rcx", "r11", "memory");
    return ret;
}

static inline i64 sys3(u64 nr, u64 a1, u64 a2, u64 a3) {
    i64 ret;
    __asm__ volatile(
        "syscall"
        : "=a"(ret)
        : "a"(nr), "D"(a1), "S"(a2), "d"(a3)
        : "rcx", "r11", "memory");
    return ret;
}

static inline i64 sys4(u64 nr, u64 a1, u64 a2, u64 a3, u64 a4) {
    i64 ret;
    register u64 r10 __asm__("r10") = a4;
    __asm__ volatile(
        "syscall"
        : "=a"(ret)
        : "a"(nr), "D"(a1), "S"(a2), "d"(a3), "r"(r10)
        : "rcx", "r11", "memory");
    return ret;
}

static int str_eq(const char *a, const char *b) {
    while (*a && *a == *b) {
        a++;
        b++;
    }
    return *a == *b;
}

static u64 str_len(const char *s) {
    u64 n = 0;
    while (s[n]) {
        n++;
    }
    return n;
}

__attribute__((aligned(16))) static volatile u64 g_tls_block[2] = {0x123456789abcdef0ULL, 42};

static u64 read_fs_qword0(void) {
    u64 val = 0;
    __asm__ volatile("movq %%fs:0, %0" : "=r"(val) : : "memory");
    return val;
}

void lx_main(void) {
    static const char start_msg[] = "LX_HELLO_START\n";
    sys3(SYS_WRITE, 1, (u64)start_msg, sizeof(start_msg) - 1);

    i64 pid = sys1(SYS_GETPID, 0);
    if (pid <= 0) {
        sys1(SYS_EXIT_GROUP, 11);
    }

    i64 brk0 = sys1(SYS_BRK, 0);
    i64 brk1 = sys1(SYS_BRK, (u64)(brk0 + 4096));
    if (brk0 <= 0 || brk1 < brk0 + 4096) {
        sys1(SYS_EXIT_GROUP, 12);
    }
    volatile u32 *heap = (volatile u32 *)(u64)brk0;
    heap[0] = 0x44525939U;
    if (heap[0] != 0x44525939U) {
        sys1(SYS_EXIT_GROUP, 13);
    }

    struct utsname_min u;
    if (sys1(SYS_UNAME, (u64)&u) != 0 || !str_eq(u.sysname, "DRY-OS")) {
        sys1(SYS_EXIT_GROUP, 14);
    }

    if (sys2(SYS_ARCH_PRCTL, ARCH_SET_FS, (u64)&g_tls_block[0]) != 0) {
        sys1(SYS_EXIT_GROUP, 15);
    }
    if (read_fs_qword0() != 0x123456789abcdef0ULL) {
        sys1(SYS_EXIT_GROUP, 16);
    }

    static const char path[] = "/ram/lx_out.txt";
    i64 fd = sys4(SYS_OPENAT, (u64)AT_FDCWD, (u64)path, (u64)(O_WRONLY | O_CREAT | O_TRUNC), 0644);
    if (fd < 3) {
        sys1(SYS_EXIT_GROUP, 17);
    }
    static const char fdata[] = "HELLO_FROM_LINUX_ELF brk=OK tls=OK\n";
    if (sys3(SYS_WRITE, (u64)fd, (u64)fdata, sizeof(fdata) - 1) != (i64)(sizeof(fdata) - 1)) {
        sys1(SYS_EXIT_GROUP, 18);
    }
    if (sys1(SYS_CLOSE, (u64)fd) != 0) {
        sys1(SYS_EXIT_GROUP, 19);
    }

    static const char ok_msg[] = "LX_HELLO_PASS tls=1 brk=1 fd=1 os=";
    sys3(SYS_WRITE, 1, (u64)ok_msg, sizeof(ok_msg) - 1);
    sys3(SYS_WRITE, 1, (u64)u.sysname, str_len(u.sysname));
    static const char nl[] = "\n";
    sys3(SYS_WRITE, 1, (u64)nl, 1);

    sys1(SYS_EXIT_GROUP, 42);
    for (;;) {}
}

__asm__(
    ".text\n"
    ".globl _start\n"
    "_start:\n"
    "    xor %rbp, %rbp\n"
    "    and $-16, %rsp\n"
    "    call lx_main\n"
    "1:\n"
    "    mov $231, %eax\n"
    "    mov $99, %edi\n"
    "    syscall\n"
    "    jmp 1b\n");
