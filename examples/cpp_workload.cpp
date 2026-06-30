// A CPU-bound C++ workload to profile.
//
// It exercises a deliberately varied call graph so a sampling profiler has lots
// to chew on: recursion, a deep call pipeline, virtual dispatch over a small
// class hierarchy, several template instantiations, std::function indirection,
// and a handful of numeric/string kernels. Hot functions are marked NOINLINE so
// they survive optimization and show up as distinct frames in the flame graph.
//
// Usage: cpp_workload [seconds]   (default 8)

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <functional>
#include <memory>
#include <string>
#include <vector>

#if defined(_MSC_VER)
#define NOINLINE __declspec(noinline)
#else
#define NOINLINE __attribute__((noinline))
#endif

// Keeps the optimizer from deleting work whose result we'd otherwise discard.
static volatile std::uint64_t g_sink = 0;

namespace {

// Tiny deterministic PRNG (xorshift64*), so runs are reproducible and cheap.
struct Rng {
    std::uint64_t state;
    explicit Rng(std::uint64_t seed) : state(seed ? seed : 0x9e3779b97f4a7c15ULL) {}
    std::uint64_t next() {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        return state * 0x2545F4914F6CDD1DULL;
    }
    int range(int lo, int hi) {
        return lo + static_cast<int>(next() % static_cast<std::uint64_t>(hi - lo));
    }
};

// ---------------------------------------------------------------------------
// Numeric kernels
// ---------------------------------------------------------------------------

NOINLINE std::uint64_t matrix_multiply_trace(int n, Rng& rng) {
    std::vector<double> a(static_cast<std::size_t>(n) * n);
    std::vector<double> b(static_cast<std::size_t>(n) * n);
    for (auto& x : a) x = rng.range(0, 9);
    for (auto& x : b) x = rng.range(0, 9);

    double trace = 0.0;
    for (int i = 0; i < n; ++i) {
        for (int j = 0; j < n; ++j) {
            double sum = 0.0;
            for (int k = 0; k < n; ++k) {
                sum += a[i * n + k] * b[k * n + j];
            }
            if (i == j) trace += sum;
        }
    }
    return static_cast<std::uint64_t>(trace);
}

NOINLINE std::uint64_t count_primes(int limit) {
    std::uint64_t count = 0;
    for (int n = 2; n < limit; ++n) {
        bool prime = true;
        for (int d = 2; static_cast<long long>(d) * d <= n; ++d) {
            if (n % d == 0) {
                prime = false;
                break;
            }
        }
        count += prime ? 1u : 0u;
    }
    return count;
}

NOINLINE int partition(std::vector<int>& v, int lo, int hi) {
    int pivot = v[(lo + hi) / 2];
    int i = lo - 1;
    int j = hi + 1;
    for (;;) {
        do { ++i; } while (v[i] < pivot);
        do { --j; } while (v[j] > pivot);
        if (i >= j) return j;
        std::swap(v[i], v[j]);
    }
}

NOINLINE void quicksort(std::vector<int>& v, int lo, int hi) {
    if (lo >= hi) return;
    int p = partition(v, lo, hi);
    quicksort(v, lo, p);
    quicksort(v, p + 1, hi);
}

NOINLINE std::uint64_t sort_kernel(int count, Rng& rng) {
    std::vector<int> v(static_cast<std::size_t>(count));
    for (auto& x : v) x = rng.range(0, 1 << 20);
    quicksort(v, 0, static_cast<int>(v.size()) - 1);
    return static_cast<std::uint64_t>(v[v.size() / 2]);
}

NOINLINE std::uint64_t hash_strings(int count, Rng& rng) {
    std::uint64_t acc = 1469598103934665603ULL;  // FNV-1a offset basis
    for (int i = 0; i < count; ++i) {
        std::string s = "item-" + std::to_string(rng.range(0, 1 << 16));
        s += "-tag-" + std::to_string(rng.range(0, 128));
        std::sort(s.begin(), s.end());
        for (unsigned char c : s) {
            acc ^= c;
            acc *= 1099511628211ULL;  // FNV-1a prime
        }
    }
    return acc;
}

// ---------------------------------------------------------------------------
// Virtual dispatch over a small shape hierarchy
// ---------------------------------------------------------------------------

struct Shape {
    virtual ~Shape() = default;
    virtual double measure() const = 0;
};

struct Circle : Shape {
    double r;
    explicit Circle(double r) : r(r) {}
    NOINLINE double measure() const override { return 3.14159265358979 * r * r; }
};

struct Square : Shape {
    double s;
    explicit Square(double s) : s(s) {}
    NOINLINE double measure() const override { return s * s; }
};

struct Triangle : Shape {
    double base, height;
    Triangle(double b, double h) : base(b), height(h) {}
    NOINLINE double measure() const override { return 0.5 * base * height; }
};

NOINLINE std::uint64_t sum_shapes(const std::vector<std::unique_ptr<Shape>>& shapes) {
    double total = 0.0;
    for (const auto& shape : shapes) {
        total += shape->measure();  // virtual call
    }
    return static_cast<std::uint64_t>(total);
}

// ---------------------------------------------------------------------------
// Template instantiations: one distinct symbol per element type
// ---------------------------------------------------------------------------

template <typename T>
NOINLINE T accumulate_work(const std::vector<T>& xs) {
    T acc = T{};
    for (const T& x : xs) {
        acc += static_cast<T>(std::sqrt(static_cast<double>(x) + 1.0));
    }
    return acc;
}

NOINLINE std::uint64_t templated_kernel(int count, Rng& rng) {
    std::vector<int> ints(static_cast<std::size_t>(count));
    std::vector<float> floats(static_cast<std::size_t>(count));
    std::vector<double> doubles(static_cast<std::size_t>(count));
    for (int i = 0; i < count; ++i) {
        int v = rng.range(0, 1000);
        ints[i] = v;
        floats[i] = static_cast<float>(v);
        doubles[i] = static_cast<double>(v);
    }
    auto a = static_cast<std::uint64_t>(accumulate_work(ints));
    auto b = static_cast<std::uint64_t>(accumulate_work(floats));
    auto c = static_cast<std::uint64_t>(accumulate_work(doubles));
    return a + b + c;
}

// ---------------------------------------------------------------------------
// A deep, named call pipeline, to guarantee tall stacks in the profile
// ---------------------------------------------------------------------------

NOINLINE std::uint64_t pipeline_leaf(std::uint64_t x) {
    for (int i = 0; i < 64; ++i) x = x * 6364136223846793005ULL + 1442695040888963407ULL;
    return x;
}
NOINLINE std::uint64_t pipeline_stage5(std::uint64_t x) { return pipeline_leaf(x ^ 5); }
NOINLINE std::uint64_t pipeline_stage4(std::uint64_t x) { return pipeline_stage5(x ^ 4); }
NOINLINE std::uint64_t pipeline_stage3(std::uint64_t x) { return pipeline_stage4(x ^ 3); }
NOINLINE std::uint64_t pipeline_stage2(std::uint64_t x) { return pipeline_stage3(x ^ 2); }
NOINLINE std::uint64_t pipeline_stage1(std::uint64_t x) { return pipeline_stage2(x ^ 1); }

NOINLINE std::uint64_t pipeline_kernel(int iters, Rng& rng) {
    std::uint64_t acc = 0;
    for (int i = 0; i < iters; ++i) acc ^= pipeline_stage1(rng.next());
    return acc;
}

// ---------------------------------------------------------------------------
// Recursive tree traversal, for variable-depth stacks
// ---------------------------------------------------------------------------

struct Node {
    std::uint64_t value;
    std::vector<std::unique_ptr<Node>> children;
};

NOINLINE std::unique_ptr<Node> build_tree(int depth, Rng& rng) {
    auto node = std::make_unique<Node>();
    node->value = rng.next();
    if (depth > 0) {
        int fanout = rng.range(1, 4);
        for (int i = 0; i < fanout; ++i) {
            node->children.push_back(build_tree(depth - 1, rng));
        }
    }
    return node;
}

NOINLINE std::uint64_t tree_sum(const Node& node) {
    std::uint64_t total = node.value;
    for (const auto& child : node.children) {
        total += tree_sum(*child);  // recursion
    }
    return total;
}

NOINLINE std::uint64_t tree_kernel(int depth, Rng& rng) {
    auto root = build_tree(depth, rng);
    return tree_sum(*root);
}

}  // namespace

int main(int argc, char** argv) {
    const int seconds = argc > 1 ? std::atoi(argv[1]) : 8;
    Rng rng(0xC0FFEEULL);

    // A pool of shapes reused across iterations for the virtual-dispatch kernel.
    std::vector<std::unique_ptr<Shape>> shapes;
    for (int i = 0; i < 4096; ++i) {
        switch (i % 3) {
            case 0: shapes.push_back(std::make_unique<Circle>(i % 17 + 1)); break;
            case 1: shapes.push_back(std::make_unique<Square>(i % 13 + 1)); break;
            default: shapes.push_back(std::make_unique<Triangle>(i % 11 + 1, i % 7 + 1)); break;
        }
    }

    // Named tasks, dispatched through std::function (adds an indirection frame).
    using Task = std::function<std::uint64_t(Rng&)>;
    std::vector<std::pair<const char*, Task>> tasks = {
        {"matrix", [](Rng& r) { return matrix_multiply_trace(64, r); }},
        {"primes", [](Rng& r) { return count_primes(40000 + r.range(0, 4000)); }},
        {"sort", [](Rng& r) { return sort_kernel(20000, r); }},
        {"strings", [](Rng& r) { return hash_strings(4000, r); }},
        {"templates", [](Rng& r) { return templated_kernel(20000, r); }},
        {"pipeline", [](Rng& r) { return pipeline_kernel(20000, r); }},
        {"tree", [](Rng& r) { return tree_kernel(13, r); }},
        {"shapes", [&shapes](Rng&) { return sum_shapes(shapes); }},
    };

    const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(seconds);
    std::uint64_t checksum = 0;
    std::uint64_t iterations = 0;
    while (std::chrono::steady_clock::now() < deadline) {
        for (auto& task : tasks) {
            checksum ^= task.second(rng);
            g_sink = checksum;
            ++iterations;
        }
    }

    std::printf("ran %llu task iterations in %ds, checksum=%llu\n",
                static_cast<unsigned long long>(iterations), seconds,
                static_cast<unsigned long long>(checksum));
    return 0;
}
