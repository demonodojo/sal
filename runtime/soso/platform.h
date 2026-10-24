#ifndef SAL_SOSO_PLATFORM_H
#define SAL_SOSO_PLATFORM_H
/* Private freestanding platform interface. No Linux libc or syscalls. */
#include <stdint.h>
#include <stddef.h>
#include <stdarg.h>
#define SAL_SOSO_API_VERSION 1
#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2
#define R_OK 4
#define PATH_MAX 4096
typedef struct { int64_t fd; int error; } FILE;
extern FILE *sal_os_stdout, *sal_os_stderr;
#define stdout sal_os_stdout
#define stderr sal_os_stderr
#define malloc sal_os_malloc
#define realloc sal_os_realloc
#define free sal_os_free
#define fopen sal_os_fopen
#define fclose sal_os_fclose
#define fread sal_os_fread
#define fwrite sal_os_fwrite
#define fseek sal_os_fseek
#define ftell sal_os_ftell
#define fflush sal_os_fflush
#define getenv sal_os_getenv
#define mkdir sal_os_mkdir
#define stat sal_os_stat
#define access sal_os_access
#define getpid sal_os_getpid
#define exit sal_os_exit
#define strtod sal_os_strtod
struct stat { uint64_t st_mode; };
#define S_ISDIR(m) ((m) == 2)
void *malloc(size_t);
void *realloc(void *, size_t);
void free(void *);
FILE *fopen(const char *, const char *);
int fclose(FILE *);
size_t fread(void *, size_t, size_t, FILE *);
size_t fwrite(const void *, size_t, size_t, FILE *);
int fseek(FILE *, long, int);
long ftell(FILE *);
int fflush(FILE *);
char *getenv(const char *);
int mkdir(const char *, int);
int stat(const char *, struct stat *);
int access(const char *, int);
int getpid(void);
_Noreturn void exit(int);
double strtod(const char *, char **);
float expf(float);
double sqrt(double);
double pow(double, double);
double sin(double);
double cos(double);
#define clock_gettime sal_os_clock_gettime
#define CLOCK_MONOTONIC 1
struct timespec { int64_t tv_sec; int64_t tv_nsec; };
int clock_gettime(int, struct timespec *);
size_t sal_os_format_f64(double, char *, size_t);
void *memcpy(void *, const void *, size_t);
void *memmove(void *, const void *, size_t);
void *memset(void *, int, size_t);
int memcmp(const void *, const void *, size_t);
size_t strlen(const char *);
int strcmp(const char *, const char *);
int strncmp(const char *, const char *, size_t);
char *strstr(const char *, const char *);
char *strchr(const char *, int);
char *strrchr(const char *, int);
int snprintf(char *, size_t, const char *, ...);
int printf(const char *, ...);
int fprintf(FILE *, const char *, ...);
int64_t sal_os_spawn(const char *, const char *const *, size_t);
char *sal_os_capture(const char *, const char *);
char *sal_os_realpath(const char *);
#endif
