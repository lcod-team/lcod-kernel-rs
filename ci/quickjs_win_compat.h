#ifndef LCOD_QJS_WIN_COMPAT_H
#define LCOD_QJS_WIN_COMPAT_H

#ifdef _WIN32
#  include <winsock2.h>
#  include <windows.h>
#  include <malloc.h>

#  ifndef alloca
#    define alloca _alloca
#  endif

#endif /* _WIN32 */

#endif /* LCOD_QJS_WIN_COMPAT_H */
