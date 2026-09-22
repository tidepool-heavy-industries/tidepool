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

static int fail(const char *path, const char *operation) {
    const char *target = getenv("JOURNAL_FAULT_PATH");
    const char *kind = getenv("JOURNAL_FAULT_KIND");
    const char *armed = getenv("JOURNAL_FAULT_ARMED");
    if (!target || !kind || !armed || strcmp(target, path) ||
        strcmp(kind, operation) || access(armed, F_OK)) return 0;
    const char *log = getenv("JOURNAL_FAULT_LOG");
    int fd = syscall(SYS_openat, AT_FDCWD, log, O_WRONLY|O_CREAT|O_APPEND, 0600);
    if (fd >= 0) {
        syscall(SYS_write, fd, "hit\n", 4);
        syscall(SYS_close, fd);
    }
    errno = EIO;
    return 1;
}

int open64(const char *path, int flags, ...) {
    mode_t mode = 0;
    if (flags & O_CREAT) {
        va_list args; va_start(args, flags); mode = va_arg(args, int); va_end(args);
    }
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
