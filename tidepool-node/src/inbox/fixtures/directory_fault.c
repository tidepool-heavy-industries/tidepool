#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>
#include <stdio.h>
/* Armed only inside a single-test subprocess, after its real setup succeeds. */
static int matches(const char *path, const char *kind) {
    const char *target = getenv("INBOX_FAULT_PATH");
    const char *mode = getenv("INBOX_FAULT_KIND");
    const char *arm = getenv("INBOX_FAULT_ARM");
    return target && mode && arm && !access(arm,F_OK) &&
        !strcmp(path,target) && !strcmp(kind,mode);
}
static int skip_open(void) {
    static unsigned int seen = 0;
    const char *skip = getenv("INBOX_FAULT_SKIP_OPEN");
    return seen++ < (skip ? strtoul(skip, NULL, 10) : 0);
}
static void hit(void) {
    const char *path=getenv("INBOX_FAULT_LOG");
    int fd=syscall(SYS_openat,AT_FDCWD,path,O_WRONLY|O_CREAT|O_APPEND,0600);
    if(fd>=0){syscall(SYS_write,fd,"hit\n",4);syscall(SYS_close,fd);}
}
int open64(const char *path,int flags,...) {
    mode_t mode=0;
    if(flags&O_CREAT){va_list args;va_start(args,flags);mode=va_arg(args,int);va_end(args);}
    int (*real)(const char*,int,...)=dlsym(RTLD_NEXT,"open64");
    if(matches(path,"open") && !skip_open()){hit();errno=EIO;return -1;}
    return real(path,flags,mode);
}
int fsync(int fd) {
    int (*real)(int)=dlsym(RTLD_NEXT,"fsync");
    char proc[64],path[4096];
    snprintf(proc,sizeof(proc),"/proc/self/fd/%d",fd);
    ssize_t n=readlink(proc,path,sizeof(path)-1);
    if(n>=0){path[n]=0;if(matches(path,"sync")){hit();errno=EIO;return -1;}}
    return real(fd);
}
