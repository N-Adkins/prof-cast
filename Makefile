.PHONY: header
header:
	cbindgen crates/profcast-ffi --config cbindgen.toml --output include/profcast.h

.PHONY: check-header
check-header: header
	git diff --exit-code include/profcast.h

.PHONY: miri-setup
miri-setup:
	rustup toolchain install nightly --profile minimal --component miri
	cargo +nightly miri setup

.PHONY: miri
miri:
	cargo +nightly miri test --workspace --all-features --locked

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
