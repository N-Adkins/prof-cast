.PHONY: header
header:
	cbindgen crates/profcast-ffi --config cbindgen.toml --output include/profcast.h

.PHONY: check-header
check-header: header
	git diff --exit-code include/profcast.h
