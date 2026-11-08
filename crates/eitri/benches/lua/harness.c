/* T011 Lua 5.4 baseline: the same workloads and method as
 * `benches/source_to_result.rs`. For each `fixtures/<name>.lua`: load
 * (`luaL_loadbuffer`, Lua's compile) and first execution (`lua_pcall`) of
 * fresh source, timed separately, in a reused state (one per workload,
 * warmed by 1,000 runs) and in a fresh state per sample (creation timed
 * separately). The script reports its result through `emit`, checked
 * every run. Garbage collection runs as Lua schedules it and is included.
 *
 * Then a long-running process per workload, as in the Eitri harness: load
 * once and run the loaded chunk LONG_RUNS times in one state; the total
 * includes loading, and steady execution is the median of the last 90% of
 * runs. Medians of LONG_REPEATS processes.
 *
 * Run: `crates/eitri/benches/lua/run.sh [samples]` */

#include <lauxlib.h>
#include <lua.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

/* The baseline is Lua 5.4. A Homebrew alias can point `lua@5.4` at a newer
 * Lua, so refuse to build against anything else rather than measure it. */
#if LUA_VERSION_NUM != 504
#error "the T011 baseline needs Lua 5.4 headers; see run.sh"
#endif

static long long emitted;
static int has_emitted;

static int emit(lua_State *L) {
    emitted = luaL_checkinteger(L, 1);
    has_emitted = 1;
    return 0;
}

static int id(lua_State *L) {
    lua_pushinteger(L, luaL_checkinteger(L, 1));
    return 1;
}

static unsigned long long now(void) { return clock_gettime_nsec_np(CLOCK_UPTIME_RAW); }

static lua_State *initialized(void) {
    lua_State *L = luaL_newstate();
    lua_register(L, "emit", emit);
    lua_register(L, "id", id);
    return L;
}

typedef struct {
    unsigned long long compile, execute;
} Sample;

static Sample sample(lua_State *L, const char *source, size_t len, const char *name, long long answer) {
    has_emitted = 0;
    unsigned long long t0 = now();
    if (luaL_loadbuffer(L, source, len, name) != LUA_OK) {
        fprintf(stderr, "%s: %s\n", name, lua_tostring(L, -1));
        exit(1);
    }
    unsigned long long t1 = now();
    if (lua_pcall(L, 0, 0, 0) != LUA_OK) {
        fprintf(stderr, "%s: %s\n", name, lua_tostring(L, -1));
        exit(1);
    }
    unsigned long long t2 = now();
    if (!has_emitted || emitted != answer) {
        fprintf(stderr, "%s: wrong answer %lld, expected %lld\n", name, emitted, answer);
        exit(1);
    }
    return (Sample){t1 - t0, t2 - t1};
}

static int compare(const void *a, const void *b) {
    unsigned long long x = *(const unsigned long long *)a, y = *(const unsigned long long *)b;
    return (x > y) - (x < y);
}

static void stats(unsigned long long *v, size_t n, unsigned long long out[3]) {
    qsort(v, n, sizeof *v, compare);
    out[0] = v[(n - 1) * 50 / 100];
    out[1] = v[(n - 1) * 99 / 100];
    out[2] = v[0];
}

static void fmt(char *buf, unsigned long long v) {
    if (v >= 10000)
        snprintf(buf, 32, "%.1f µs", v / 1000.0);
    else
        snprintf(buf, 32, "%llu ns", v);
}

#define LONG_RUNS 10000
#define LONG_REPEATS 5

/* One long-running process; writes its total and steady execution. */
static void long_run(const char *source, size_t len, const char *name, long long answer,
                     unsigned long long *total, unsigned long long *steady) {
    static unsigned long long times[LONG_RUNS];
    lua_State *L = initialized();
    unsigned long long start = now();
    if (luaL_loadbuffer(L, source, len, name) != LUA_OK) {
        fprintf(stderr, "%s: %s\n", name, lua_tostring(L, -1));
        exit(1);
    }
    for (size_t i = 0; i < LONG_RUNS; i++) {
        has_emitted = 0;
        lua_pushvalue(L, -1);
        unsigned long long t0 = now();
        if (lua_pcall(L, 0, 0, 0) != LUA_OK) {
            fprintf(stderr, "%s: %s\n", name, lua_tostring(L, -1));
            exit(1);
        }
        times[i] = now() - t0;
        if (!has_emitted || emitted != answer) {
            fprintf(stderr, "%s: wrong answer %lld, expected %lld\n", name, emitted, answer);
            exit(1);
        }
    }
    *total = now() - start;
    lua_close(L);
    unsigned long long s[3];
    stats(times + LONG_RUNS / 10, LONG_RUNS - LONG_RUNS / 10, s);
    *steady = s[0];
}

static const struct {
    const char *name;
    long long answer;
} WORKLOADS[] = {
    {"module", 754},
    {"arithmetic", 7914953978373240671LL},
    {"branching", 1622},
    {"calls", 610},
    {"host_calls", 499500},
};

int main(int argc, char **argv) {
    const char *dir = argv[1];
    size_t samples = argc > 2 ? strtoul(argv[2], NULL, 10) : 20001;
    const char *csv_path = argc > 3 ? argv[3] : NULL;
    FILE *csv = csv_path ? fopen(csv_path, "w") : NULL;
    if (csv) fprintf(csv, "workload,case,compile_ns,execute_ns\n");
    Sample *runs = malloc(samples * sizeof *runs);
    unsigned long long *values = malloc(samples * sizeof *values);
    unsigned long long *creation = malloc(samples * sizeof *creation);

    printf("# lua %s, samples per cell: %zu\n\n", LUA_RELEASE, samples);
    printf("| workload | bytes | case | compile p50 / p99 | execute p50 / p99 | total p50 / p99 | total min |\n");
    printf("|---|---|---|---|---|---|---|\n");
    for (size_t w = 0; w < sizeof WORKLOADS / sizeof *WORKLOADS; w++) {
        const char *name = WORKLOADS[w].name;
        long long answer = WORKLOADS[w].answer;
        char path[1024];
        snprintf(path, sizeof path, "%s/%s.lua", dir, name);
        FILE *f = fopen(path, "rb");
        if (!f) { perror(path); return 1; }
        fseek(f, 0, SEEK_END);
        size_t len = (size_t)ftell(f);
        fseek(f, 0, SEEK_SET);
        char *source = malloc(len);
        if (fread(source, 1, len, f) != len) { perror(path); return 1; }
        fclose(f);

        for (int c = 0; c < 2; c++) {
            const char *kase = c == 0 ? "reused" : "fresh";
            lua_State *L = NULL;
            if (c == 0) {
                L = initialized();
                for (int i = 0; i < 1000; i++) sample(L, source, len, name, answer);
            }
            for (size_t i = 0; i < samples; i++) {
                if (c == 1) {
                    unsigned long long t0 = now();
                    L = initialized();
                    creation[i] = now() - t0;
                }
                runs[i] = sample(L, source, len, name, answer);
                if (c == 1) lua_close(L);
                if (csv) fprintf(csv, "%s,%s,%llu,%llu\n", name, kase, runs[i].compile, runs[i].execute);
            }
            if (c == 0) lua_close(L);
            unsigned long long s[3][3];
            for (size_t i = 0; i < samples; i++) values[i] = runs[i].compile;
            stats(values, samples, s[0]);
            for (size_t i = 0; i < samples; i++) values[i] = runs[i].execute;
            stats(values, samples, s[1]);
            for (size_t i = 0; i < samples; i++) values[i] = runs[i].compile + runs[i].execute;
            stats(values, samples, s[2]);
            char b[7][32];
            fmt(b[0], s[0][0]); fmt(b[1], s[0][1]); fmt(b[2], s[1][0]); fmt(b[3], s[1][1]);
            fmt(b[4], s[2][0]); fmt(b[5], s[2][1]); fmt(b[6], s[2][2]);
            printf("| %s | %zu | %s | %s / %s | %s / %s | %s / %s | %s |\n", name, len, kase, b[0], b[1], b[2],
                   b[3], b[4], b[5], b[6]);
        }
        unsigned long long cs[3];
        stats(creation, samples, cs);
        char b0[32], b1[32];
        fmt(b0, cs[0]); fmt(b1, cs[1]);
        printf("| %s | | state creation | | | %s / %s | |\n", name, b0, b1);
        free(source);
    }
    if (csv) fclose(csv);

    printf("\n| workload | %d runs: total / execute p50 |\n|---|---|\n", LONG_RUNS);
    for (size_t w = 0; w < sizeof WORKLOADS / sizeof *WORKLOADS; w++) {
        const char *name = WORKLOADS[w].name;
        char path[1024];
        snprintf(path, sizeof path, "%s/%s.lua", dir, name);
        FILE *f = fopen(path, "rb");
        if (!f) { perror(path); return 1; }
        fseek(f, 0, SEEK_END);
        size_t len = (size_t)ftell(f);
        fseek(f, 0, SEEK_SET);
        char *source = malloc(len);
        if (fread(source, 1, len, f) != len) { perror(path); return 1; }
        fclose(f);
        unsigned long long totals[LONG_REPEATS], steadies[LONG_REPEATS], t[3], st[3];
        for (int r = 0; r < LONG_REPEATS; r++)
            long_run(source, len, name, WORKLOADS[w].answer, &totals[r], &steadies[r]);
        stats(totals, LONG_REPEATS, t);
        stats(steadies, LONG_REPEATS, st);
        char b0[32], b1[32];
        fmt(b0, t[0]); fmt(b1, st[0]);
        printf("| %s | %s / %s |\n", name, b0, b1);
        free(source);
    }
    return 0;
}
