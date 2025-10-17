#ifndef LCOD_QJS_WIN_COMPAT_H
#define LCOD_QJS_WIN_COMPAT_H

#ifdef _WIN32
#  include <winsock2.h>
#  include <windows.h>
#  include <sys/timeb.h>
#  include <malloc.h>

#  ifndef alloca
#    define alloca _alloca
#  endif

static inline int qjs_gettimeofday(struct timeval *tv) {
    struct _timeb tb;
    if (_ftime_s(&tb) != 0) {
        return -1;
    }
    tv->tv_sec = (long)tb.time;
    tv->tv_usec = (long)tb.millitm * 1000;
    return 0;
}

#  ifndef HAVE_GETTIMEOFDAY
#    define HAVE_GETTIMEOFDAY 1
#    define gettimeofday(tv, tz) (void)(tz), qjs_gettimeofday((tv))
#  endif

#endif /* _WIN32 */

#endif /* LCOD_QJS_WIN_COMPAT_H */
