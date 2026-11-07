# C API

**Status:** proposed. Names and signatures may change until the first
release; the semantics follow [Embedding](embedding.md).

`eitri.h` is Eitri's **stable ABI**. C and C++ hosts use it directly, and
bindings for other languages are built on it. The [Rust API](rust-api.md)
doesn't go through it.

## Conventions

- Every name starts with `eitri_`; constants with `EITRI_`.
- **Every fallible function returns `eitri_status`.** `EITRI_OK` is zero.
  Details of the last failure are in `eitri_last_error(rt)`.
- **No unwinding crosses the boundary.** There is no `longjmp`. A C++ host
  must not let exceptions escape a host function.
- **Strings are `eitri_str`**: a pointer and a length, UTF-8, not
  NUL-terminated.
- **Handles are `eitri_handle`**, a 64-bit slot index plus generation number.
  Stale handles are detected, never reused silently.
- **Threading:** a runtime may be used from one thread at a time. Moving a
  runtime between threads is allowed only if everything the host registered
  (userdata, objects, allocator, loader) is safe to use from the new thread;
  that is the host's responsibility. Separate runtimes are independent.
- Pointers returned by the API stay valid until the stated point (usually
  the end of the current host call) and are owned by the runtime.

```c
typedef struct eitri_runtime eitri_runtime;
typedef struct eitri_module  eitri_module;
typedef struct eitri_call    eitri_call;
typedef uint64_t             eitri_handle;
typedef uint64_t             eitri_token;
typedef struct { const char *ptr; size_t len; } eitri_str;

typedef enum {
    EITRI_OK = 0,
    EITRI_ERR_COMPILE,       /* diagnostics available */
    EITRI_ERR_REGISTER,      /* binding doesn't match its declaration */
    EITRI_ERR_TYPE,          /* wrong accessor for the value's type */
    EITRI_ERR_STALE,         /* stale handle or spent token */
    EITRI_ERR_MEMORY,
    EITRI_ERR_VERSION,
    EITRI_ERR_STATE          /* call not allowed in the current state */
} eitri_status;
```

## Versioning

```c
#define EITRI_API_VERSION 1
uint32_t eitri_version(void);
```

`eitri_runtime_new` fails with `EITRI_ERR_VERSION` if the library and the
header the host was built with are incompatible.

## Runtime lifecycle

```c
typedef void *(*eitri_alloc_fn)(void *userdata, size_t size, size_t align);
typedef void  (*eitri_free_fn)(void *userdata, void *ptr, size_t size, size_t align);
typedef eitri_status (*eitri_loader_fn)(void *userdata, eitri_str path,
                                        eitri_str *source_out);

typedef struct {
    uint32_t        api_version;         /* set by eitri_config_default */
    size_t          memory_limit;
    eitri_alloc_fn  alloc;               /* NULL: the system allocator */
    eitri_free_fn   free;
    void           *alloc_userdata;
    uint64_t        budget_steps;
    uint64_t        cleanup_budget_steps;
    size_t          cleanup_reserve;
    uint32_t        instantiation_limit;
    uint32_t        std_modules;         /* EITRI_STD_COMPILE | EITRI_STD_JSON | ... */
    eitri_loader_fn loader;
    void           *loader_userdata;
} eitri_config;

eitri_config eitri_config_default(void);
eitri_status eitri_runtime_new(const eitri_config *config, eitri_runtime **out);
void         eitri_runtime_free(eitri_runtime *rt);
const char  *eitri_last_error(eitri_runtime *rt);
```

## Registering host modules

```c
typedef eitri_status (*eitri_host_fn)(eitri_call *call, void *userdata);
typedef void         (*eitri_drop_fn)(void *object, void *userdata);

eitri_module *eitri_module_begin(eitri_runtime *rt, const char *name,
                                 eitri_str declaration_source);
void eitri_module_function(eitri_module *m, const char *name,
                           eitri_host_fn fn, void *userdata);
void eitri_module_async_function(eitri_module *m, const char *name,
                                 eitri_host_fn fn, void *userdata);
void eitri_module_property(eitri_module *m, const char *class_name,
                           const char *field, eitri_host_fn getter,
                           eitri_host_fn setter, void *userdata);   /* setter may be NULL for `pub let` */
void eitri_module_method(eitri_module *m, const char *class_name,
                         const char *method, eitri_host_fn fn, void *userdata);
void eitri_module_on_unreferenced(eitri_module *m, const char *class_name,
                                  eitri_drop_fn fn, void *userdata);
void eitri_module_value_object(eitri_module *m, const char *name,
                               const char *class_name, void *object);
eitri_status eitri_module_end(eitri_module *m);
```

`eitri_module_end` checks every binding against the declaration file. On
`EITRI_ERR_REGISTER` or `EITRI_ERR_COMPILE`, the diagnostics are available
through `eitri_last_diagnostics(rt)`.

## Inside a host function

A host function receives the call context and its registered `userdata`.
Arguments are numbered from 0 in declaration order; defaults are already
filled in.

| Eitri type | Read an argument | Return a value |
|---|---|---|
| `i8`, `i64` | `eitri_arg_i64` | `eitri_return_i64` |
| `u8`, `u64` | `eitri_arg_u64` | `eitri_return_u64` |
| `f32`, `f64` | `eitri_arg_f64` | `eitri_return_f64` |
| `bool` | `eitri_arg_bool` | `eitri_return_bool` |
| `char` | `eitri_arg_char` (a `uint32_t` scalar) | `eitri_return_char` |
| `string` | `eitri_arg_str` (valid until the call returns) | `eitri_return_str` (copied) |
| `bytes` | `eitri_arg_bytes` | `eitri_return_bytes` (copied) |
| `?T` | `eitri_arg_is_null`, then the `T` accessor | `eitri_return_null`, or the `T` accessor |
| Payload-less `enum` | `eitri_arg_enum` (the case index) | `eitri_return_enum` |
| `struct`, `enum` with payloads | `eitri_arg_struct(call, i, &out, sizeof out)` | `eitri_return_struct` |
| `@extern struct` | `eitri_arg_struct`, which copies the C-layout value | `eitri_return_struct` |
| `[]T` of an `@extern struct` | `eitri_arg_extern_array(call, i, &ptr, &len)`, borrowed with no copy | — |
| `@extern class` | `eitri_arg_object` (the pointer to the object's memory) | `eitri_return_object` |
| Host `class` | `eitri_arg_object` (the host pointer) | `eitri_return_object` |
| References and callbacks | `eitri_arg_handle` | `eitri_return_handle` |

Out-of-range indexes and accessors that don't match the declared type return
`EITRI_ERR_TYPE` in debug builds.

- **`E!T`:** call `eitri_return_ok` before writing a `T`, or `eitri_return_err`
  before writing the error value. The error must be an Eitri type from the
  declaration.
- **Faulting:** `return eitri_fault(call, "message");` faults the calling
  coroutine as if it had called `panic`. The trace names the host function.
- **Host objects:** in property getters, setters, and methods,
  `eitri_self(call)` returns the host pointer of the object being accessed.
- **Host state:** use the `userdata` registered with the binding.
- **The runtime:** `eitri_call_runtime(call)` returns the runtime, for
  retaining handles or calling back into scripts during the call.

### Mirror structs: `eitri-bindgen`

```
eitri-bindgen host/editor.eit > editor_eitri.h
```

`eitri-bindgen` reads a declaration file and generates C mirror structs,
enum case constants, and typed wrappers such as
`editor_arg_Position(call, i, &pos)`. The generated layout matches what
`eitri_arg_struct` writes, so hosts never compute offsets by hand. For
`@extern` types, it also emits `static_assert`s on size, alignment, and every
field offset, so a header that drifts from its declaration fails to compile.

## Borrowing and reentrancy

Pointers obtained during a host call (from `eitri_arg_str`,
`eitri_arg_bytes`, `eitri_arg_extern_array`, `eitri_arg_object`, and
`eitri_self`) are valid until the host function **returns or calls back into
the runtime**, whichever comes first. Any reentrant call (`eitri_invoke`,
`eitri_start`, `eitri_resume_end`, `eitri_cancel`, `eitri_drain`, or a
script lookup) invalidates them; fetch them again afterwards. Handles stay
valid across reentry.

Debug builds keep an epoch counter that every reentrant call increments, and
each pointer accessor records the epoch it was fetched in, so using a stale
pointer through the API is reported as `EITRI_ERR_STALE`. Release builds
don't check.

## Templates

```c
eitri_template *t;
eitri_template_new(&cfg, &t);
/* register modules with eitri_module_begin_template(t, ...) ... eitri_module_end */
eitri_runtime *rt;
eitri_runtime_from_template(t, &rt);   /* shares checked module descriptors */
eitri_template_free(t);                /* after every runtime made from it is freed */
```

## Handles, retaining, and callbacks

```c
eitri_status eitri_retain(eitri_runtime *rt, eitri_handle h, eitri_handle *retained);
void         eitri_release(eitri_runtime *rt, eitri_handle retained);
```

- A handle read with `eitri_arg_handle` is **borrowed until the host
  function returns**. Using it afterwards returns `EITRI_ERR_STALE`.
- `eitri_retain` returns a retained handle that stays valid until
  `eitri_release`. Retained handles are GC roots.
- Unreleased handles are reported as leaks by debug builds when the runtime
  is freed.

Calling a script function or callback:

```c
eitri_status eitri_invoke_begin(eitri_runtime *rt, eitri_handle callable);
void eitri_push_i64(eitri_runtime *rt, int64_t v);   /* and _u64, _f64, _bool, _str, ... */
eitri_status eitri_invoke(eitri_runtime *rt, eitri_outcome *out);
```

Arguments are pushed in order. `eitri_invoke` runs the call with its own
budget and fills in an outcome.

## Running scripts and reading outcomes

```c
typedef enum { EITRI_OUTCOME_OK, EITRI_OUTCOME_ERR, EITRI_OUTCOME_FAULT } eitri_outcome_kind;

typedef struct {
    eitri_outcome_kind kind;
    /* opaque; read with the accessors below */
} eitri_outcome;

eitri_status eitri_run(eitri_runtime *rt, eitri_str name, eitri_str source,
                       eitri_outcome *out);

int64_t     eitri_outcome_i64(const eitri_outcome *o);   /* and _u64, _f64, _str, _handle, ... */
eitri_str   eitri_outcome_error_message(const eitri_outcome *o);
eitri_str   eitri_outcome_fault_message(const eitri_outcome *o);
eitri_str   eitri_outcome_fault_trace(const eitri_outcome *o);
size_t      eitri_outcome_secondary_faults(const eitri_outcome *o);
void        eitri_outcome_free(eitri_runtime *rt, eitri_outcome *o);
```

`eitri_run` returns `EITRI_ERR_COMPILE` if the script doesn't compile, with
diagnostics in `eitri_last_diagnostics(rt)`. Otherwise it returns `EITRI_OK`
and the outcome says whether the script succeeded, returned an `Error`, or
faulted.

### Looking up script functions

```c
eitri_status eitri_function_lookup(eitri_runtime *rt, const char *module,
                                   const char *name, const char *type,
                                   eitri_handle *out);
```

The expected type is written as an Eitri type, such as `"fn(i64) -> bool"`,
and checked once at lookup. The returned handle is retained; call it with
`eitri_invoke_begin`.

## Async host functions and tasks

```c
eitri_status eitri_suspend(eitri_call *call, eitri_token *token);
void         eitri_on_cancel(eitri_call *call, eitri_token token,
                             void (*hook)(void *userdata), void *userdata);

eitri_status eitri_resume_begin(eitri_runtime *rt, eitri_token token);
/* then write the result with the eitri_return_* functions, using the runtime: */
void         eitri_resume_return_i64(eitri_runtime *rt, int64_t v);   /* and the others */
eitri_status eitri_resume_end(eitri_runtime *rt, eitri_report *report);

eitri_status eitri_cancel(eitri_runtime *rt, eitri_token token, eitri_report *report);
eitri_status eitri_start(eitri_runtime *rt, eitri_handle async_callable, eitri_report *report);
eitri_status eitri_drain(eitri_runtime *rt, eitri_report *report);
```

An async host function either returns a value normally (the fast path, with
no suspension) or calls `eitri_suspend` and returns `EITRI_OK` without
writing a result. Tokens are one-shot; reusing one returns `EITRI_ERR_STALE`.
An `eitri_report` lists the primary task's ID and outcome, other tasks that
finished, secondary faults, and the ready-queue length.

## Worked example: binding the editor

Part of a C binding for [`host/editor.eit`](../examples/editor-config/host/editor.eit):

```c
#include "eitri.h"
#include "editor_eitri.h"          /* generated by eitri-bindgen */

static eitri_status editor_map(eitri_call *call, void *ud) {
    Editor *ed = ud;
    int64_t   mode   = eitri_arg_enum(call, 0);
    eitri_str keys   = eitri_arg_str(call, 1);
    eitri_handle action;
    eitri_status st = eitri_retain(eitri_call_runtime(call),
                                   eitri_arg_handle(call, 2), &action);
    if (st != EITRI_OK) return st;
    eitri_str desc   = eitri_arg_str(call, 3);
    int64_t buffer   = eitri_arg_is_null(call, 4) ? -1 : eitri_arg_i64(call, 4);
    keymap_insert(ed, mode, keys.ptr, keys.len, buffer, action, desc.ptr, desc.len);
    return EITRI_OK;
}

static eitri_status editor_read_file(eitri_call *call, void *ud) {
    eitri_str path = eitri_arg_str(call, 0);
    FileBuf buf;
    if (read_whole_file(path.ptr, path.len, &buf) != 0) {
        editor_IoError err = editor_IoError_NotFound(path);   /* generated */
        eitri_return_err(call);
        return editor_return_IoError(call, &err);
    }
    eitri_return_ok(call);
    eitri_return_str(call, (eitri_str){ buf.data, buf.len });
    file_buf_free(&buf);
    return EITRI_OK;
}

static eitri_status options_get_number(eitri_call *call, void *ud) {
    Options *o = eitri_self(call);
    eitri_return_bool(call, o->number);
    return EITRI_OK;
}

static eitri_status options_set_number(eitri_call *call, void *ud) {
    Options *o = eitri_self(call);
    o->number = eitri_arg_bool(call, 0);
    request_redraw(ud);
    return EITRI_OK;
}

eitri_status register_editor(eitri_runtime *rt, Editor *ed) {
    eitri_module *m = eitri_module_begin(rt, "editor",
        (eitri_str){ EDITOR_DECL, sizeof EDITOR_DECL - 1 });
    eitri_module_function(m, "map", editor_map, ed);
    eitri_module_function(m, "read_file", editor_read_file, ed);
    eitri_module_property(m, "Options", "number",
                          options_get_number, options_set_number, ed);
    /* ... one binding per declaration ... */
    eitri_module_value_object(m, "options", "Options", &ed->options);
    return eitri_module_end(m);
}

/* When the user presses a mapped key: */
void on_key(eitri_runtime *rt, eitri_handle action) {
    eitri_outcome out;
    if (eitri_invoke_begin(rt, action) == EITRI_OK &&
        eitri_invoke(rt, &out) == EITRI_OK) {
        if (out.kind == EITRI_OUTCOME_FAULT) {
            eitri_str trace = eitri_outcome_fault_trace(&out);
            show_error(trace.ptr, trace.len);
        }
        eitri_outcome_free(rt, &out);
    }
}
```
