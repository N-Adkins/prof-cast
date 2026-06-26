.PHONY: header
header:
	cbindgen crates/profcast-ffi --config cbindgen.toml --output include/profcast.h

.PHONY: check-header
check-header: header
	git diff --exit-code include/profcast.h

# Fuzzing. Requires a nightly toolchain and cargo-fuzz (`make fuzz-install`).
# Run a target with `make fuzz t=folded_read`. Optionally time-box a run with
# `make fuzz t=folded_read secs=120`; without `secs` it runs until you Ctrl-C.
.PHONY: fuzz-install
fuzz-install:
	cargo install cargo-fuzz

.PHONY: fuzz-build
fuzz-build:
	cargo +nightly fuzz build

.PHONY: fuzz-list
fuzz-list:
	cargo +nightly fuzz list

.PHONY: fuzz
fuzz:
	@if [ -z "$(target)" ]; then \
		echo "Usage: make fuzz target=<target> [secs=<seconds>]"; \
		echo "Available targets:"; \
		cargo +nightly fuzz list; \
		exit 2; \
	fi
	cargo +nightly fuzz run $(target) $(if $(secs),-- -max_total_time=$(secs))
