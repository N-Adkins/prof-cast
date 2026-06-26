#ifndef PROFCAST_H
#define PROFCAST_H

#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>

/**
 * Returns a null-terminated string showing the program name and version,
 * eg. "profcast 0.1.0".
 *
 * In the case of an internal library error for this, it will
 * return "profcast <unknown>".
 */
const char *profcast_version(void);

#endif  /* PROFCAST_H */
