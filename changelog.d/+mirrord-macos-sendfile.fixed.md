Fixed `sendfile` on macOS reporting a failed send as fully sent when the socket would block, which made callers like Ruby's
 `IO.copy_stream` silently drop data.
