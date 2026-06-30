# Example profiles

Real captured profiles for exercising profcast end to end — parsing, transcoding,
and visualization — on data with the messy distribution a real profiler produces,
rather than a hand-written fixture.

## `bs4_scrape.folded`

A CPU profile of [BeautifulSoup4](https://www.crummy.com/software/BeautifulSoup/)
parsing and scraping a large synthetic HTML document. Captured with
[py-spy](https://github.com/benfred/py-spy) at 250 Hz for 8 seconds:

```sh
py-spy record -f raw -o bs4_scrape.folded --rate 250 --duration 8 \
    -- python3 bs4_scrape_workload.py
```

`bs4_scrape_workload.py` is the workload it was captured from, kept here so the
profile is reproducible and its provenance is clear.

## `cpp_workload.cpp`

A CPU-bound C++ program for exercising the **live capture** backends against a
native binary. It runs a deliberately varied call graph — recursion, a deep call
pipeline, virtual dispatch, several template instantiations, `std::function`
indirection, and numeric/string kernels — so the resulting flame graph has plenty
of distinct, named frames and varied depth.

Build it and capture a profile with the `justfile` here (needs
[`just`](https://github.com/casey/just), [CMake](https://cmake.org), and a C++
compiler):

```sh
just build      # or, from the repo root: just build-examples
just record     # builds, captures out.speedscope.json, open at speedscope.app
```

CMake locates the default toolchain on its own (MSVC on Windows, `cc`/`c++` on
Unix). To pick another, pass it configure flags — run `just clean` first when
switching, since CMake caches the compiler:

```sh
just build "-D CMAKE_CXX_COMPILER=clang++"          # Linux/macOS: clang (or g++)
just build "-T ClangCL"                              # Windows: clang-cl
just build "-G Ninja -D CMAKE_CXX_COMPILER=clang++"  # Windows: GNU clang++
```

The compile/debug flags live in `CMakeLists.txt`, which sets them per compiler so
the binary always carries the debug info the sampler needs: `-g` on Linux/macOS, a
**PDB** on Windows (MSVC/clang-cl `/Zi`, or GNU clang's `-gcodeview`). Stripped
binaries — or ones with only an export table — symbolize poorly, since the sampler
can then only guess from the nearest exported symbol.

## Try it

```sh
# Inspect the parsed internal model.
profcast dump examples/bs4_scrape.folded

# Transcode for the speedscope.app flamegraph viewer.
profcast convert examples/bs4_scrape.folded out.speedscope.json --to speedscope

# Transcode to pprof (e.g. for `go tool pprof`).
profcast convert examples/bs4_scrape.folded out.pb.gz --to pprof
```
