#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

/* Activation is a file owned by one subprocess, not mutable process environment. */
static int armed(void) { return access(getenv("BIND_FAULT_ARM"), F_OK) == 0; }
static int matches(const char *path) { return !strcmp(path, getenv("BIND_FAULT_PATH")); }
static void hit(void) {
    int fd = syscall(SYS_openat, AT_FDCWD, getenv("BIND_FAULT_LOG"), O_WRONLY|O_CREAT|O_APPEND, 0600);
    if (fd >= 0) { syscall(SYS_write, fd, "hit\n", 4); syscall(SYS_close, fd); }
}
int open64(const char *path, int flags, ...) {
    mode_t mode = 0;
    if (flags & O_CREAT) { va_list args; va_start(args, flags); mode = va_arg(args, int); va_end(args); }
    int (*real)(const char*, int, ...) = dlsym(RTLD_NEXT, "open64");
    if (armed() && matches(path) && !strcmp(getenv("BIND_FAULT_KIND"), "open")) {
        hit(); errno = EIO; return -1;
    }
    return real(path, flags, mode);
}
int fsync(int fd) {
    int (*real)(int) = dlsym(RTLD_NEXT, "fsync");
    char proc[64], path[4096];
    snprintf(proc, sizeof(proc), "/proc/self/fd/%d", fd);
    ssize_t n = readlink(proc, path, sizeof(path) - 1);
    if (n >= 0) path[n] = 0;
    if (armed() && n >= 0 && matches(path) && !strcmp(getenv("BIND_FAULT_KIND"), "sync")) {
        hit(); errno = EIO; return -1;
    }
    return real(fd);
}
