/* Emula la ABI de syscalls de soso en Linux para `sal build --target linux`. */
#ifndef SAL_SOSO

#include "../sal_runtime.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <netdb.h>
#include <poll.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/random.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

enum {
    SOSO_SYS_READ = 1,
    SOSO_SYS_WRITE = 2,
    SOSO_SYS_OPEN = 3,
    SOSO_SYS_CLOSE = 4,
    SOSO_SYS_STAT = 6,
    SOSO_SYS_GETDENTS = 7,
    SOSO_SYS_MKDIR = 8,
    SOSO_SYS_UNLINK = 9,
    SOSO_SYS_WAIT = 11,
    SOSO_SYS_SLEEP_MS = 13,
    SOSO_SYS_PIPE = 21,
    SOSO_SYS_SPAWN_IO = 22,
    SOSO_SYS_CHDIR = 23,
    SOSO_SYS_GETCWD = 24,
    SOSO_SYS_TCP_CONNECT = 29,
    SOSO_SYS_TCP_LISTEN = 30,
    SOSO_SYS_TCP_ACCEPT = 31,
    SOSO_SYS_READ_TIMEOUT = 32,
    SOSO_SYS_DNS_RESOLVE = 40,
    SOSO_SYS_KILL = 52,
    SOSO_SYS_RENAME = 70,
    SOSO_SYS_GETENV = 83,
    SOSO_SYS_CLOCK_GETTIME = 72,
    SOSO_SYS_GETRANDOM = 78,
    SOSO_SYS_UPTIME_MS = 28,
    SOSO_CLOCK_REALTIME = 0,
    SOSO_CLOCK_MONOTONIC = 1,
    SOSO_O_RDONLY = 0,
    SOSO_O_WRONLY = 1,
    SOSO_O_APPEND = 2,
    SOSO_O_CREAT = 4,
    SOSO_O_TRUNC = 8,
    SOSO_FT_FILE = 1,
    SOSO_FT_DIR = 2,
    SOSO_DIRENT_SIZE = 272,
    SOSO_SPAWNIO_SIZE = 112,
    SOSO_FD_INHERIT = (int64_t)-1,
    SOSO_FD_CLOSED = (int64_t)-12,
};

typedef struct {
    uint64_t ino;
    uint8_t file_type;
    uint8_t name_len;
    uint8_t name[255];
    uint8_t _pad[7];
} SosoDirent;

typedef struct {
    uint64_t ino;
    uint64_t size;
    uint64_t mtime;
    uint8_t file_type;
    uint8_t _pad[7];
} SosoStat;

typedef struct {
    uint8_t addr[4];
    uint16_t port;
    uint16_t _pad;
} SosoSockAddr;

typedef struct {
    uint64_t path_ptr;
    uint64_t path_len;
    uint64_t args_ptr;
    uint64_t args_len;
    uint64_t stdin_fd;
    uint64_t stdout_fd;
    uint64_t stderr_fd;
    uint64_t argv_ptr;
    uint64_t argv_count;
    uint64_t envp_ptr;
    uint64_t envp_count;
    uint64_t log_fd;
    uint64_t cwd_ptr;
    uint64_t cwd_len;
} SosoSpawnIo;

static int64_t neg_errno(void) {
    return -(int64_t)errno;
}

static int copy_user_str(uint64_t ptr, uint64_t len, char *out, size_t cap) {
    if (len >= cap || ptr == 0) {
        errno = EINVAL;
        return -1;
    }
    memcpy(out, (const void *)(uintptr_t)ptr, (size_t)len);
    out[len] = '\0';
    return 0;
}

static int map_open_flags(int64_t flags) {
    int f = 0;
    if ((flags & SOSO_O_WRONLY) != 0) {
        f |= O_WRONLY;
    } else {
        f |= O_RDONLY;
    }
    if ((flags & SOSO_O_CREAT) != 0 || (flags & 64) != 0) {
        f |= O_CREAT;
    }
    if ((flags & SOSO_O_TRUNC) != 0) {
        f |= O_TRUNC;
    }
    if ((flags & SOSO_O_APPEND) != 0) {
        f |= O_APPEND;
    }
    return f;
}

#define DIR_SLOTS 64
static struct {
    int fd;
    off_t cookie;
} g_dir_slots[DIR_SLOTS];

static off_t *dir_cookie_for(int fd, int create) {
    for (int i = 0; i < DIR_SLOTS; i++) {
        if (g_dir_slots[i].fd == fd) {
            return &g_dir_slots[i].cookie;
        }
    }
    if (!create) {
        return NULL;
    }
    for (int i = 0; i < DIR_SLOTS; i++) {
        if (g_dir_slots[i].fd == 0) {
            g_dir_slots[i].fd = fd;
            g_dir_slots[i].cookie = 0;
            return &g_dir_slots[i].cookie;
        }
    }
    return NULL;
}

static void dir_clear_fd(int fd) {
    for (int i = 0; i < DIR_SLOTS; i++) {
        if (g_dir_slots[i].fd == fd) {
            g_dir_slots[i].fd = 0;
            g_dir_slots[i].cookie = 0;
        }
    }
}

static int64_t soso_getdents(int fd, uint64_t buf, int64_t len) {
    if (len < SOSO_DIRENT_SIZE) {
        errno = EINVAL;
        return neg_errno();
    }
    int cfd = dup(fd);
    if (cfd < 0) {
        return neg_errno();
    }
    DIR *d = fdopendir(cfd);
    if (!d) {
        close(cfd);
        return neg_errno();
    }
    off_t *cookie = dir_cookie_for(fd, 1);
    if (!cookie) {
        closedir(d);
        errno = EMFILE;
        return neg_errno();
    }
    seekdir(d, (long)*cookie);
    struct dirent *ent = readdir(d);
    if (!ent) {
        closedir(d);
        return 0;
    }
    (*cookie)++;
    SosoDirent out = {0};
    out.ino = (uint64_t)ent->d_ino;
    size_t nlen = strlen(ent->d_name);
    if (nlen > 255) {
        nlen = 255;
    }
    out.name_len = (uint8_t)nlen;
    memcpy(out.name, ent->d_name, nlen);
    if (ent->d_type == DT_DIR) {
        out.file_type = SOSO_FT_DIR;
    } else {
        out.file_type = SOSO_FT_FILE;
    }
    memcpy((void *)(uintptr_t)buf, &out, sizeof(out));
    closedir(d);
    return SOSO_DIRENT_SIZE;
}

static void child_map_stdio(uint64_t spec, int slot) {
    if (spec == (uint64_t)SOSO_FD_INHERIT) {
        return;
    }
    if (spec == (uint64_t)SOSO_FD_CLOSED) {
        int nullfd = open("/dev/null", O_RDWR);
        if (nullfd >= 0) {
            dup2(nullfd, slot);
            close(nullfd);
        }
        return;
    }
    dup2((int)spec, slot);
}

static int read_argv_entry(uint64_t pair, char *buf, size_t cap) {
    uint64_t s_ptr = 0;
    uint64_t s_len = 0;
    memcpy(&s_ptr, (void *)(uintptr_t)pair, 8);
    memcpy(&s_len, (void *)(uintptr_t)(pair + 8), 8);
    return copy_user_str(s_ptr, s_len, buf, cap);
}

static int64_t soso_spawn_io(uint64_t opts_ptr) {
    if (opts_ptr == 0) {
        errno = EFAULT;
        return neg_errno();
    }
    SosoSpawnIo opts;
    memcpy(&opts, (void *)(uintptr_t)opts_ptr, sizeof(opts));
    char path[4096];
    if (copy_user_str(opts.path_ptr, opts.path_len, path, sizeof(path)) != 0) {
        return neg_errno();
    }
    char *argv[256];
    int argc = 0;
    static char slots[256][4096];
    if (opts.argv_ptr != 0 && opts.argv_count > 0) {
        if (opts.argv_count >= (int)(sizeof(argv) / sizeof(argv[0]))) {
            errno = E2BIG;
            return neg_errno();
        }
        for (uint64_t i = 0; i < opts.argv_count; i++) {
            if (read_argv_entry(opts.argv_ptr + i * 16, slots[i], sizeof(slots[i])) != 0) {
                return neg_errno();
            }
            argv[argc++] = slots[i];
        }
    } else {
        argv[argc++] = path;
    }
    if (opts.argv_ptr == 0 && opts.args_len != 0) {
        char line[4096];
        if (copy_user_str(opts.args_ptr, opts.args_len, line, sizeof(line)) != 0) {
            return neg_errno();
        }
        char *save = NULL;
        for (char *tok = strtok_r(line, " \t", &save); tok; tok = strtok_r(NULL, " \t", &save)) {
            if (argc >= (int)(sizeof(argv) / sizeof(argv[0])) - 1) {
                break;
            }
            argv[argc++] = tok;
        }
    }
    argv[argc] = NULL;

    pid_t pid = fork();
    if (pid < 0) {
        return neg_errno();
    }
    if (pid == 0) {
        if (opts.cwd_len > 0) {
            char cwd[4096];
            if (copy_user_str(opts.cwd_ptr, opts.cwd_len, cwd, sizeof(cwd)) == 0) {
                (void)chdir(cwd);
            }
        }
        child_map_stdio(opts.stdin_fd, STDIN_FILENO);
        child_map_stdio(opts.stdout_fd, STDOUT_FILENO);
        child_map_stdio(opts.stderr_fd, STDERR_FILENO);
        execvp(path, argv);
        _exit(127);
    }
    return (int64_t)pid;
}

static int64_t tcp_connect_linux(uint64_t addr_ptr, int64_t timeout_ms) {
    SosoSockAddr sa;
    memcpy(&sa, (void *)(uintptr_t)addr_ptr, sizeof(sa));
    int s = socket(AF_INET, SOCK_STREAM, 0);
    if (s < 0) {
        return neg_errno();
    }
    struct sockaddr_in in = {0};
    in.sin_family = AF_INET;
    memcpy(&in.sin_addr, sa.addr, 4);
    in.sin_port = htons((uint16_t)sa.port);
    int flags = fcntl(s, F_GETFL, 0);
    if (flags >= 0) {
        (void)fcntl(s, F_SETFL, flags | O_NONBLOCK);
    }
    int rc = connect(s, (struct sockaddr *)&in, sizeof(in));
    if (rc < 0 && errno != EINPROGRESS) {
        close(s);
        return neg_errno();
    }
    if (rc < 0) {
        struct pollfd pfd = {.fd = s, .events = POLLOUT};
        int pr = poll(&pfd, 1, (int)timeout_ms);
        if (pr <= 0) {
            close(s);
            errno = pr == 0 ? ETIMEDOUT : errno;
            return neg_errno();
        }
        int err = 0;
        socklen_t el = sizeof(err);
        if (getsockopt(s, SOL_SOCKET, SO_ERROR, &err, &el) != 0 || err != 0) {
            close(s);
            errno = err ? err : ECONNREFUSED;
            return neg_errno();
        }
    }
    if (flags >= 0) {
        (void)fcntl(s, F_SETFL, flags);
    }
    return (int64_t)s;
}

int64_t sal_os_syscall(int64_t nr, int64_t a0, int64_t a1, int64_t a2, int64_t a3, int64_t a4,
                       int64_t a5) {
    (void)a4;
    (void)a5;
    switch (nr) {
    case SOSO_SYS_READ:
        return (int64_t)read((int)a0, (void *)(uintptr_t)a1, (size_t)a2);
    case SOSO_SYS_WRITE:
        return (int64_t)write((int)a0, (const void *)(uintptr_t)a1, (size_t)a2);
    case SOSO_SYS_CLOSE:
        dir_clear_fd((int)a0);
        return (int64_t)close((int)a0);
    case SOSO_SYS_OPEN: {
        char path[4096];
        if (copy_user_str((uint64_t)a0, (uint64_t)a1, path, sizeof(path)) != 0) {
            return neg_errno();
        }
        return (int64_t)open(path, map_open_flags(a2), 0644);
    }
    case SOSO_SYS_STAT: {
        char path[4096];
        if (copy_user_str((uint64_t)a0, (uint64_t)a1, path, sizeof(path)) != 0) {
            return neg_errno();
        }
        struct stat st;
        if (stat(path, &st) != 0) {
            return neg_errno();
        }
        SosoStat out = {0};
        out.ino = (uint64_t)st.st_ino;
        out.size = (uint64_t)st.st_size;
        out.mtime = (uint64_t)st.st_mtime;
        if (S_ISDIR(st.st_mode)) {
            out.file_type = SOSO_FT_DIR;
        } else {
            out.file_type = SOSO_FT_FILE;
        }
        uint64_t cap = (uint64_t)a3;
        if (cap < sizeof(out)) {
            errno = EINVAL;
            return neg_errno();
        }
        memcpy((void *)(uintptr_t)a2, &out, sizeof(out));
        return 0;
    }
    case SOSO_SYS_GETDENTS:
        return soso_getdents((int)a0, (uint64_t)a1, a2);
    case SOSO_SYS_MKDIR: {
        char path[4096];
        if (copy_user_str((uint64_t)a0, (uint64_t)a1, path, sizeof(path)) != 0) {
            return neg_errno();
        }
        return mkdir(path, 0755) == 0 ? 0 : neg_errno();
    }
    case SOSO_SYS_UNLINK: {
        char path[4096];
        if (copy_user_str((uint64_t)a0, (uint64_t)a1, path, sizeof(path)) != 0) {
            return neg_errno();
        }
        return unlink(path) == 0 ? 0 : neg_errno();
    }
    case SOSO_SYS_RENAME: {
        char oldp[4096];
        char newp[4096];
        if (copy_user_str((uint64_t)a0, (uint64_t)a1, oldp, sizeof(oldp)) != 0) {
            return neg_errno();
        }
        if (copy_user_str((uint64_t)a2, (uint64_t)a3, newp, sizeof(newp)) != 0) {
            return neg_errno();
        }
        return rename(oldp, newp) == 0 ? 0 : neg_errno();
    }
    case SOSO_SYS_WAIT: {
        int status = 0;
        pid_t pid = waitpid((pid_t)a0, &status, 0);
        if (pid < 0) {
            return neg_errno();
        }
        if (WIFEXITED(status)) {
            return ((int64_t)pid << 8) | (WEXITSTATUS(status) & 0xff);
        }
        return ((int64_t)pid << 8) | 1;
    }
    case SOSO_SYS_KILL:
        return kill((pid_t)a0, (int)a1) == 0 ? 0 : neg_errno();
    case SOSO_SYS_SLEEP_MS:
        usleep((useconds_t)(a0 * 1000));
        return 0;
    case SOSO_SYS_UPTIME_MS: {
        struct timespec ts;
        if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
            return neg_errno();
        }
        return (int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
    }
    case SOSO_SYS_PIPE: {
        int fds[2];
        if (pipe(fds) != 0) {
            return neg_errno();
        }
        return ((int64_t)fds[1] << 32) | (uint32_t)fds[0];
    }
    case SOSO_SYS_CHDIR: {
        char path[4096];
        if (copy_user_str((uint64_t)a0, (uint64_t)a1, path, sizeof(path)) != 0) {
            return neg_errno();
        }
        return chdir(path) == 0 ? 0 : neg_errno();
    }
    case SOSO_SYS_GETCWD: {
        char path[4096];
        if (getcwd(path, sizeof(path)) == NULL) {
            return neg_errno();
        }
        size_t n = strlen(path);
        if ((uint64_t)a1 < n + 1) {
            errno = EINVAL;
            return neg_errno();
        }
        memcpy((void *)(uintptr_t)a0, path, n + 1);
        return (int64_t)a0;
    }
    case SOSO_SYS_CLOCK_GETTIME: {
        clockid_t clk = CLOCK_REALTIME;
        if (a0 == SOSO_CLOCK_MONOTONIC) {
            clk = CLOCK_MONOTONIC;
        } else if (a0 != SOSO_CLOCK_REALTIME) {
            errno = EINVAL;
            return neg_errno();
        }
        struct timespec ts;
        if (clock_gettime(clk, &ts) != 0) {
            return neg_errno();
        }
        int64_t out[2] = {ts.tv_sec, ts.tv_nsec};
        memcpy((void *)(uintptr_t)a1, out, sizeof(out));
        return 0;
    }
    case SOSO_SYS_GETRANDOM: {
        if (a1 == 0) {
            return 0;
        }
        ssize_t n = getrandom((void *)(uintptr_t)a0, (size_t)a1, 0);
        if (n < 0) {
            return neg_errno();
        }
        return (int64_t)n;
    }
    case SOSO_SYS_READ_TIMEOUT: {
        struct pollfd pfd = {.fd = (int)a0, .events = POLLIN};
        int pr = poll(&pfd, 1, (int)a3);
        if (pr < 0) {
            return neg_errno();
        }
        if (pr == 0) {
            errno = EAGAIN;
            return neg_errno();
        }
        return (int64_t)read((int)a0, (void *)(uintptr_t)a1, (size_t)a2);
    }
    case SOSO_SYS_TCP_CONNECT:
        return tcp_connect_linux((uint64_t)a0, a1);
    case SOSO_SYS_TCP_LISTEN: {
        int s = socket(AF_INET, SOCK_STREAM, 0);
        if (s < 0) {
            return neg_errno();
        }
        int one = 1;
        (void)setsockopt(s, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));
        struct sockaddr_in in = {0};
        in.sin_family = AF_INET;
        in.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        in.sin_port = htons((uint16_t)a0);
        if (bind(s, (struct sockaddr *)&in, sizeof(in)) != 0 || listen(s, 8) != 0) {
            close(s);
            return neg_errno();
        }
        return (int64_t)s;
    }
    case SOSO_SYS_TCP_ACCEPT: {
        struct pollfd pfd = {.fd = (int)a0, .events = POLLIN};
        int pr = poll(&pfd, 1, (int)a1);
        if (pr < 0) {
            return neg_errno();
        }
        if (pr == 0) {
            return a0;
        }
        int c = accept((int)a0, NULL, NULL);
        return c < 0 ? neg_errno() : (int64_t)c;
    }
    case SOSO_SYS_DNS_RESOLVE: {
        char host[256];
        if (copy_user_str((uint64_t)a0, (uint64_t)a1, host, sizeof(host)) != 0) {
            return neg_errno();
        }
        struct addrinfo hints = {0}, *res = NULL;
        hints.ai_family = AF_INET;
        if (getaddrinfo(host, NULL, &hints, &res) != 0 || !res) {
            errno = ENOENT;
            return neg_errno();
        }
        struct sockaddr_in *in = (struct sockaddr_in *)res->ai_addr;
        memcpy((void *)(uintptr_t)a2, &in->sin_addr, 4);
        freeaddrinfo(res);
        return 0;
    }
    case SOSO_SYS_GETENV: {
        char name[256];
        if (copy_user_str((uint64_t)a0, (uint64_t)a1, name, sizeof(name)) != 0) {
            return neg_errno();
        }
        const char *val = getenv(name);
        if (!val) {
            return 0;
        }
        size_t n = strlen(val);
        if (n >= (size_t)a3) {
            errno = ERANGE;
            return neg_errno();
        }
        memcpy((void *)(uintptr_t)a2, val, n + 1);
        return (int64_t)n;
    }
    case SOSO_SYS_SPAWN_IO:
        return soso_spawn_io((uint64_t)a0);
    default:
        errno = ENOSYS;
        return neg_errno();
    }
}

#endif /* !SAL_SOSO */
