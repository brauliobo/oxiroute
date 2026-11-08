/* LD_PRELOAD allocator call counter for the R2 harness (valgrind is unusable on current glibc/AVX-512 hosts).
 *
 * Counts malloc/calloc/realloc/posix_memalign/aligned_alloc/memalign/free calls and requested bytes.
 * `kill -USR1 PID` appends one snapshot line to $R2_ALLOC_FILE:
 *   mallocs callocs reallocs memaligns frees bytes clock_gettime_calls live_bytes
 * The harness takes two snapshots around the measured window and divides the difference by the request count.
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <malloc.h>
#include <stdlib.h>
#include <time.h>
#include <string.h>
#include <unistd.h>

static unsigned long mallocs, callocs, reallocs, memaligns, frees, bytes, clock_calls;
static long live_bytes;
static int (*real_clock_gettime)(clockid_t, struct timespec *);
static void *(*real_malloc)(size_t);
static void *(*real_calloc)(size_t, size_t);
static void *(*real_realloc)(void *, size_t);
static int (*real_posix_memalign)(void **, size_t, size_t);
static void *(*real_aligned_alloc)(size_t, size_t);
static void *(*real_memalign)(size_t, size_t);
static void (*real_free)(void *);
static char bootstrap[65536];
static size_t bootstrap_used;
static int resolving;

#define ADD(counter, amount) __atomic_fetch_add(&(counter), (amount), __ATOMIC_RELAXED)

static void snapshot(int signal_number) {
    (void)signal_number;
    const char *path = getenv("R2_ALLOC_FILE");
    if (!path) return;
    int fd = open(path, O_WRONLY | O_APPEND | O_CREAT, 0600);
    if (fd < 0) return;
    char line[256];
    int length = snprintf(line, sizeof line, "%lu %lu %lu %lu %lu %lu %lu %ld\n", mallocs, callocs,
                          reallocs, memaligns, frees, bytes, clock_calls, live_bytes);
    if (write(fd, line, (size_t)length) < 0) { /* best effort */ }
    close(fd);
}

static void resolve(void) {
    if (real_free) return;
    resolving = 1;
    real_malloc = dlsym(RTLD_NEXT, "malloc");
    real_calloc = dlsym(RTLD_NEXT, "calloc");
    real_realloc = dlsym(RTLD_NEXT, "realloc");
    real_posix_memalign = dlsym(RTLD_NEXT, "posix_memalign");
    real_aligned_alloc = dlsym(RTLD_NEXT, "aligned_alloc");
    real_memalign = dlsym(RTLD_NEXT, "memalign");
    real_free = dlsym(RTLD_NEXT, "free");
    real_clock_gettime = dlsym(RTLD_NEXT, "clock_gettime");
    resolving = 0;
    signal(SIGUSR1, snapshot);
}

static void *early(size_t size) {
    size = (size + 15) & ~(size_t)15;
    void *pointer = bootstrap + bootstrap_used;
    bootstrap_used += size;
    return pointer;
}

static int is_early(void *pointer) {
    return (char *)pointer >= bootstrap && (char *)pointer < bootstrap + sizeof bootstrap;
}

void *malloc(size_t size) {
    if (resolving) return early(size);
    resolve();
    ADD(mallocs, 1);
    ADD(bytes, size);
    void *pointer = real_malloc(size);
    if (pointer) ADD(live_bytes, (long)malloc_usable_size(pointer));
    return pointer;
}

void *calloc(size_t count, size_t size) {
    if (resolving) return memset(early(count * size), 0, count * size);
    resolve();
    ADD(callocs, 1);
    ADD(bytes, count * size);
    void *pointer = real_calloc(count, size);
    if (pointer) ADD(live_bytes, (long)malloc_usable_size(pointer));
    return pointer;
}

void *realloc(void *pointer, size_t size) {
    resolve();
    if (is_early(pointer)) {
        void *fresh = real_malloc(size);
        memcpy(fresh, pointer, size);
        return fresh;
    }
    ADD(reallocs, 1);
    ADD(bytes, size);
    long before = pointer ? (long)malloc_usable_size(pointer) : 0;
    void *fresh = real_realloc(pointer, size);
    if (fresh) ADD(live_bytes, (long)malloc_usable_size(fresh) - before);
    return fresh;
}

int posix_memalign(void **out, size_t alignment, size_t size) {
    resolve();
    ADD(memaligns, 1);
    ADD(bytes, size);
    int result = real_posix_memalign(out, alignment, size);
    if (result == 0) ADD(live_bytes, (long)malloc_usable_size(*out));
    return result;
}

void *aligned_alloc(size_t alignment, size_t size) {
    resolve();
    ADD(memaligns, 1);
    ADD(bytes, size);
    void *pointer = real_aligned_alloc(alignment, size);
    if (pointer) ADD(live_bytes, (long)malloc_usable_size(pointer));
    return pointer;
}

void *memalign(size_t alignment, size_t size) {
    resolve();
    ADD(memaligns, 1);
    ADD(bytes, size);
    void *pointer = real_memalign(alignment, size);
    if (pointer) ADD(live_bytes, (long)malloc_usable_size(pointer));
    return pointer;
}

int clock_gettime(clockid_t clock, struct timespec *out) {
    if (!real_clock_gettime) {
        resolve();
        if (!real_clock_gettime) return -1;
    }
    ADD(clock_calls, 1);
    return real_clock_gettime(clock, out);
}

void free(void *pointer) {
    if (!pointer || is_early(pointer)) return;
    resolve();
    ADD(frees, 1);
    ADD(live_bytes, -(long)malloc_usable_size(pointer));
    real_free(pointer);
}
