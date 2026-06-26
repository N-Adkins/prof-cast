#ifndef PROFCAST_H
#define PROFCAST_H

#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>

/**
 * Opaque handle to a parsed profile produced by [`profcast_read`].
 */
struct profcast_Profile;

/**
 * Returns a null-terminated string showing the program name and version,
 * eg. "profcast 0.1.0".
 *
 * In the case of an internal library error for this, it will
 * return "profcast <unknown>".
 */
const char *profcast_version(void);

/**
 * Returns the current thread's most recent error message, or `NULL` if none.
 *
 * The returned pointer is owned by the library and remains valid until the
 * next profcast call on this thread; do not free it.
 */
const char *profcast_last_error(void);

/**
 * Detects the format of `len` bytes at `buf`, optionally hinted by `filename`.
 *
 * Returns an owned format-name string (free with [`profcast_string_free`]), or
 * `NULL` if no format matched.
 *
 * # Safety
 *
 * `buf` must be valid for `len` bytes (or null with `len` 0). `filename`, when
 * non-null, must be a valid NUL-terminated C string.
 */
char *profcast_probe(const uint8_t *buf, size_t len, const char *filename);

/**
 * Parses `len` bytes at `buf` into a profile handle.
 *
 * `format` selects an input format by name; when null the format is
 * auto-detected by probing, with `filename` used as an optional hint. Returns
 * a handle to free with [`profcast_profile_free`], or `NULL` on error (see
 * [`profcast_last_error`]).
 *
 * # Safety
 *
 * `buf` must be valid for `len` bytes (or null with `len` 0). `format` and
 * `filename`, when non-null, must be valid NUL-terminated C strings.
 */
struct profcast_Profile *profcast_read(const uint8_t *buf,
                                       size_t len,
                                       const char *format,
                                       const char *filename);

/**
 * Serializes a profile to a JSON string (free with [`profcast_string_free`]).
 *
 * Returns `NULL` on error (see [`profcast_last_error`]).
 *
 * # Safety
 *
 * `profile` must be a non-null pointer returned by [`profcast_read`] that has
 * not yet been freed.
 */
char *profcast_profile_to_json(const struct profcast_Profile *profile);

/**
 * Frees a profile handle previously returned by [`profcast_read`].
 *
 * Passing `NULL` is a no-op.
 *
 * # Safety
 *
 * `profile` must either be null or a pointer from [`profcast_read`] that has
 * not already been freed. It must not be used after this call.
 */
void profcast_profile_free(struct profcast_Profile *profile);

/**
 * Frees a string previously returned by this library.
 *
 * Passing `NULL` is a no-op.
 *
 * # Safety
 *
 * `string` must either be null or a pointer returned by a profcast function
 * (e.g. [`profcast_probe`]) that has not already been freed.
 */
void profcast_string_free(char *string);

#endif  /* PROFCAST_H */
