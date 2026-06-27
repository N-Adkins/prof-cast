.PHONY: header
header:
	cbindgen crates/profcast-ffi --config cbindgen.toml --output include/profcast.h

# Regenerate the vendored pprof prost types. Needs protoc-gen-prost
# (cargo install protoc-gen-prost). The committed output is consumed verbatim
# by src/pprof/mod.rs, which supplies the module wrapper and lint allows.
PPROF_PROTO_DIR := crates/profcast-formats/proto
PPROF_GEN := crates/profcast-formats/src/pprof/proto.gen.rs

.PHONY: proto
proto:
	@tmp=$$(mktemp -d); \
	protoc --prost_out="$$tmp" -I $(PPROF_PROTO_DIR) $(PPROF_PROTO_DIR)/profile.proto; \
	mv "$$(find "$$tmp" -name '*.rs')" $(PPROF_GEN); \
	rm -rf "$$tmp"; \
	echo "regenerated $(PPROF_GEN)"

.PHONY: check-header
check-header: header
	git diff --exit-code include/profcast.h

.PHONY: install
install:
	cargo install --path crates/profcast-cli --locked

.PHONY: uninstall
uninstall:
	cargo uninstall profcast-cli

.PHONY: lint
lint: 
	cargo lint

.PHONY: test
test:
	cargo test --all-targets

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
