#ifndef LCOD_QJS_SYS_TIME_H
#define LCOD_QJS_SYS_TIME_H

#ifdef _WIN32
#include <winsock2.h>
#include <windows.h>
#include <sys/timeb.h>
#include <malloc.h>

#ifndef alloca
#  define alloca _alloca
#endif

static inline int gettimeofday(struct timeval *tv, void *tz) {
    (void)tz;
    struct _timeb tb;
    if (_ftime_s(&tb) != 0) {
        return -1;
    }
    tv->tv_sec = (long)tb.time;
    tv->tv_usec = tb.millitm * 1000;
    return 0;
}

#else
#include_next <sys/time.h>
#endif

#endif /* LCOD_QJS_SYS_TIME_H */
