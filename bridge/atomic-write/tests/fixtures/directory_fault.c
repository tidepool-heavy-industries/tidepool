#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>
#include <stdio.h>
static int matches(const char *path) { const char *target=getenv("FAULT_PATH"); return target && !strcmp(path,target); }
static void hit(void) { const char *p=getenv("FAULT_LOG"); if(!p)return; int fd=syscall(SYS_openat,AT_FDCWD,p,O_WRONLY|O_CREAT|O_APPEND,0600); if(fd>=0){syscall(SYS_write,fd,"hit\n",4);syscall(SYS_close,fd);} }
int open64(const char *path,int flags,...) { mode_t mode=0; if(flags&O_CREAT){va_list args;va_start(args,flags);mode=va_arg(args,int);va_end(args);} int (*real)(const char*,int,...)=dlsym(RTLD_NEXT,"open64"); const char *kind=getenv("FAULT_KIND"); if(kind&&!strcmp(kind,"open")&&matches(path)){hit();errno=EIO;return -1;} return real(path,flags,mode); }
int fsync(int fd) { int (*real)(int)=dlsym(RTLD_NEXT,"fsync"); struct stat st;char proc[64],path[4096];snprintf(proc,sizeof(proc),"/proc/self/fd/%d",fd);ssize_t n=readlink(proc,path,sizeof(path)-1);if(n>=0)path[n]=0;const char *kind=getenv("FAULT_KIND");if(!fstat(fd,&st)&&S_ISDIR(st.st_mode)&&n>=0&&matches(path)){hit();if(kind&&!strcmp(kind,"sync")){errno=EIO;return -1;}}return real(fd); }
