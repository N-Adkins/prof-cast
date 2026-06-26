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
	@if [ -z "$(t)" ]; then \
		echo "Usage: make fuzz t=<target> [secs=<seconds>]"; \
		echo "Available targets:"; \
		cargo +nightly fuzz list; \
		exit 2; \
	fi
	cargo +nightly fuzz run $(t) $(if $(secs),-- -max_total_time=$(secs))
