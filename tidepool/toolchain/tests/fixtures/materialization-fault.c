#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

/* The atomic-write directory-fault seam, scoped to this child's owned tree. */
int fsync(int fd) {
    int (*real_sync)(int) = dlsym(RTLD_NEXT, "fsync");
    const char *root = getenv("MATERIALIZATION_FAULT_ROOT");
    const char *kind = getenv("MATERIALIZATION_FAULT_KIND");
    char descriptor[64], path[4096];
    struct stat metadata;
    snprintf(descriptor, sizeof(descriptor), "/proc/self/fd/%d", fd);
    ssize_t length = readlink(descriptor, path, sizeof(path) - 1);
    if (root && kind && length >= 0 && !fstat(fd, &metadata)) {
        path[length] = 0;
        size_t root_length = strlen(root);
        if (!strncmp(path, root, root_length) &&
            (path[root_length] == 0 || path[root_length] == '/')) {
            const char *log = getenv("MATERIALIZATION_FAULT_LOG");
            int output = syscall(SYS_openat, AT_FDCWD, log,
                                 O_WRONLY | O_CREAT | O_APPEND, 0600);
            if (output >= 0) {
                const char *entry = S_ISDIR(metadata.st_mode) ? "directory\n" : "file\n";
                syscall(SYS_write, output, entry, strlen(entry));
                syscall(SYS_close, output);
            }
            if ((!strcmp(kind, "file") && S_ISREG(metadata.st_mode)) ||
                (!strcmp(kind, "directory") && S_ISDIR(metadata.st_mode))) {
                errno = EIO;
                return -1;
            }
        }
    }
    return real_sync(fd);
}
