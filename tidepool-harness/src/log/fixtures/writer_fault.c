#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>
static int seen;
static int fail(const char *path, const char *kind) {
    if (strcmp(getenv("LOG_FAULT_PATH"), path) || strcmp(getenv("LOG_FAULT_KIND"), kind)) return 0;
    if (++seen != atoi(getenv("LOG_FAULT_NTH"))) return 0;
    int fd = syscall(SYS_openat, AT_FDCWD, getenv("LOG_FAULT_HITS"), O_WRONLY|O_CREAT|O_APPEND, 0600);
    if (fd >= 0) { syscall(SYS_write, fd, "hit\n", 4); syscall(SYS_close, fd); }
    errno = EIO;
    return 1;
}
int open64(const char *path, int flags, ...) {
    mode_t mode = 0;
    if (flags & O_CREAT) { va_list args; va_start(args, flags); mode = va_arg(args, int); va_end(args); }
    int (*real)(const char *, int, ...) = dlsym(RTLD_NEXT, "open64");
    if (fail(path, "open")) return -1;
    return real(path, flags, mode);
}
int fsync(int fd) {
    int (*real)(int) = dlsym(RTLD_NEXT, "fsync");
    char proc[64], path[4096];
    snprintf(proc, sizeof(proc), "/proc/self/fd/%d", fd);
    ssize_t n = readlink(proc, path, sizeof(path)-1);
    if (n >= 0) { path[n] = 0; if (fail(path, "sync")) return -1; }
    return real(fd);
}
