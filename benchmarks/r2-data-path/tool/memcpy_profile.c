/* LD_PRELOAD profile of the large memcpy/memmove calls of a process, by caller (see memcpy_profile.py).
 *
 * Counts calls of 48 bytes or more per (return address, size); sizes above 4 KiB are rounded up to 4 KiB.
 * `kill -USR1 PID` starts counting (clearing the table); the next one stops it and writes one line per entry,
 * "return-address size count", to $MEMPROF_FILE. Async state machines that are moved into a box or into the frame
 * of the future that awaits them show up as a few large copies per request.
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <stdint.h>
#define N 8192
typedef struct { uintptr_t ret; size_t size; unsigned long count; } Entry;
static Entry table[N];
static void *(*real_memcpy)(void *, const void *, size_t);
static void *(*real_memmove)(void *, const void *, size_t);
static volatile int enabled;
static uintptr_t base;
static void *boot_buf[1];
static void record(uintptr_t ret, size_t n) {
    if (!enabled || n < 48) return;
    size_t key = n > 4096 ? ((n + 4095) & ~4095UL) : n;
    uint64_t h = (ret * 0x9E3779B97F4A7C15ULL) ^ (key * 0xC2B2AE3D27D4EB4FULL);
    for (unsigned i = 0; i < N; i++) {
        Entry *e = &table[(h + i) % N];
        if (e->count == 0) { e->ret = ret; e->size = key; }
        if (e->ret == ret && e->size == key) { __atomic_fetch_add(&e->count, 1, __ATOMIC_RELAXED); return; }
    }
}
static void init(void) {
    if (!real_memcpy) { real_memcpy = dlsym(RTLD_NEXT, "memcpy"); real_memmove = dlsym(RTLD_NEXT, "memmove"); }
}
void *memcpy(void *d, const void *s, size_t n) { init(); record((uintptr_t)__builtin_return_address(0), n); return real_memcpy(d, s, n); }
void *memmove(void *d, const void *s, size_t n) { init(); record((uintptr_t)__builtin_return_address(0), n); return real_memmove(d, s, n); }
static void handler(int sig) {
    (void)sig;
    if (!enabled) { memset(table, 0, sizeof table); enabled = 1; return; }
    enabled = 0;
    const char *path = getenv("MEMPROF_FILE");
    FILE *f = fopen(path, "w");
    if (!f) return;
    for (unsigned i = 0; i < N; i++) if (table[i].count) fprintf(f, "%lx %zu %lu\n", table[i].ret, table[i].size, table[i].count);
    fclose(f);
}
__attribute__((constructor)) static void setup(void) { signal(SIGUSR1, handler); }
