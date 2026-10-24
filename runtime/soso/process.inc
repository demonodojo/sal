/* Included after string helpers, so returned strings retain Sal's header. */
char *sal_realpath(const char *path) {
    char *p = sal_os_realpath(path);
    char *s = sal_str_from_cstr(p ? p : "");
    free(p);
    return s;
}
char *sal_tmp_path(const char *suffix) {
    char buf[512];
    snprintf(buf, sizeof(buf), "/tmp/sal-%d-%s", getpid(), suffix ? suffix : "tmp");
    return sal_str_from_cstr(buf);
}
char *sal_exec_capture(const char *bin, const char *arg) {
    char *p = sal_os_capture(bin, arg);
    char *s = sal_str_from_cstr(p ? p : "");
    free(p);
    return s;
}
int64_t sal_exec_compile(const char *bin, const char *src, const char *out) {
    const char *args[] = {bin, src, "-o", out};
    return sal_os_spawn(bin, args, 4);
}
/* The C-emitting selfhost compiler is a separate, future port. */
int64_t sal_clang(const char *a, const char *b) {
    (void)a; (void)b; sal_panic("compilador C de autohospedaje no disponible en soso"); return 1;
}
int64_t sal_clang_obj(const char *a, const char *b) { return sal_clang(a, b); }
int64_t sal_link_objs(const char *a, const char *b) { return sal_clang(a, b); }
