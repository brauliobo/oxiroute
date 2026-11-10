/* LD_PRELOAD histogram of the libc memory-function calls of a process: calls and bytes per size class.
 *
 * Functions: memcpy memmove memset memcmp bcmp memchr strlen. Sizes 0..64 are counted exactly, larger ones per power of
 * two. `kill -USR1 PID` starts counting (clearing the table); the next one stops it and writes
 * "function size calls" lines to $LIBCHIST_FILE.
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#undef memchr
#define F 7
#define B 80
static const char *names[F] = {"memcpy", "memmove", "memset", "memcmp", "bcmp", "memchr", "strlen"};
static unsigned long calls[F][B];
static void *real[F];
static volatile int enabled;
static __thread int busy;

static unsigned bucket(size_t n) {
    if (n <= 64) return n;
    return 64 + (63 - __builtin_clzl(n)) - 6 + 1;
}
static void count(int f, size_t n) {
    if (enabled) __atomic_fetch_add(&calls[f][bucket(n)], 1, __ATOMIC_RELAXED);
}
static void *resolve(int f) {
    if (!real[f]) { busy = 1; real[f] = dlsym(RTLD_NEXT, names[f]); busy = 0; }
    return real[f];
}
/* dlsym itself may call these before the real function is resolved: serve them with plain loops. */
void *memcpy(void *d, const void *s, size_t n) {
    if (busy) { char *a = d; const char *b = s; while (n--) *a++ = *b++; return d; }
    void *(*r)(void *, const void *, size_t) = resolve(0);
    count(0, n);
    return r(d, s, n);
}
void *memmove(void *d, const void *s, size_t n) {
    if (busy) { char *a = d; const char *b = s; if (a < b) while (n--) *a++ = *b++; else while (n--) a[n] = b[n]; return d; }
    void *(*r)(void *, const void *, size_t) = resolve(1);
    count(1, n);
    return r(d, s, n);
}
void *memset(void *d, int c, size_t n) {
    if (busy) { char *a = d; while (n--) *a++ = c; return d; }
    void *(*r)(void *, int, size_t) = resolve(2);
    count(2, n);
    return r(d, c, n);
}
int memcmp(const void *x, const void *y, size_t n) {
    if (busy) { const unsigned char *a = x, *b = y; for (; n; n--, a++, b++) if (*a != *b) return *a - *b; return 0; }
    int (*r)(const void *, const void *, size_t) = resolve(3);
    count(3, n);
    return r(x, y, n);
}
int bcmp(const void *x, const void *y, size_t n) {
    if (busy) { const unsigned char *a = x, *b = y; for (; n; n--, a++, b++) if (*a != *b) return 1; return 0; }
    int (*r)(const void *, const void *, size_t) = resolve(4);
    count(4, n);
    return r(x, y, n);
}
void *memchr(const void *s, int c, size_t n) {
    if (busy) { const unsigned char *a = s; for (; n; n--, a++) if (*a == (unsigned char)c) return (void *)a; return 0; }
    void *(*r)(const void *, int, size_t) = resolve(5);
    count(5, n);
    return r(s, c, n);
}
size_t strlen(const char *s) {
    if (busy) { size_t n = 0; while (s[n]) n++; return n; }
    size_t (*r)(const char *) = resolve(6);
    size_t n = r(s);
    count(6, n);
    return n;
}
static void handler(int sig) {
    (void)sig;
    if (!enabled) { memset(calls, 0, sizeof calls); enabled = 1; return; }
    enabled = 0;
    FILE *f = fopen(getenv("LIBCHIST_FILE"), "w");
    if (!f) return;
    for (int i = 0; i < F; i++)
        for (int j = 0; j < B; j++)
            if (calls[i][j]) fprintf(f, "%s %d %lu\n", names[i], j <= 64 ? j : 1 << (j - 64 + 6), calls[i][j]);
    fclose(f);
}
__attribute__((constructor)) static void setup(void) { signal(SIGUSR1, handler); }
