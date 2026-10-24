#include "platform.h"
size_t strlen(const char *s) { size_t n=0; while(s[n]) n++; return n; }
int strcmp(const char *a,const char *b) { while(*a && *a==*b) {a++;b++;} return (unsigned char)*a-(unsigned char)*b; }
int strncmp(const char *a,const char *b,size_t n) { for(size_t i=0;i<n;i++) { int d=(unsigned char)a[i]-(unsigned char)b[i]; if(d || !a[i]) return d; } return 0; }
char *strchr(const char *s,int c) { do { if(*s==(char)c) return (char*)s; } while(*s++); return NULL; }
char *strrchr(const char *s,int c) { const char *r=NULL; do { if(*s==(char)c) r=s; } while(*s++); return (char*)r; }
char *strstr(const char *s,const char *n) { size_t k=strlen(n); if(!k) return (char*)s; for(;*s;s++) if(!strncmp(s,n,k)) return (char*)s; return NULL; }
