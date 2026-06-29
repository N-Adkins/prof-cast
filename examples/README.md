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

## Try it

```sh
# Inspect the parsed internal model.
profcast dump examples/bs4_scrape.folded

# Transcode for the speedscope.app flamegraph viewer.
profcast convert examples/bs4_scrape.folded out.speedscope.json --to speedscope

# Transcode to pprof (e.g. for `go tool pprof`).
profcast convert examples/bs4_scrape.folded out.pb.gz --to pprof
```
