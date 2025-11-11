# llama-cpp-rs-main - Modifications for Shared GGML

## Overview

This document describes how to modify `llama-cpp-rs-main` (specifically the `llama-cpp-sys-2` and `llama-cpp-2` crates) to use the shared GGML library from `ggml-sys` instead of building its own embedded GGML. This allows `llama-cpp-2` and `whisper-rs` to share the same GGML library, avoiding duplicate symbol conflicts.

## Prerequisites

- Fork of `llama-cpp-rs-main` repository (or the specific crates)
- `ggml-sys` crate set up and available (see `GGML_SYS_SETUP.md`)
- Understanding of Rust build scripts and CMake

## Repository Structure

```
llama-cpp-rs-main/
├── llama-cpp-sys-2/
│   ├── Cargo.toml
│   ├── build.rs
│   ├── wrapper.h
│   └── llama.cpp/  # llama.cpp source code
└── llama-cpp-2/
    ├── Cargo.toml
    └── src/
        └── lib.rs
```

## Step 1: Add `use-shared-ggml` Feature to `llama-cpp-2/Cargo.toml`

Add the feature to the main crate's `Cargo.toml`:

```toml
[features]
# ... existing features ...
# Use shared GGML backend to avoid duplicate symbol conflicts
use-shared-ggml = ["llama-cpp-sys-2/use-shared-ggml"]
```

## Step 2: Modify `llama-cpp-sys-2/Cargo.toml`

### 2.1 Add `ggml-sys` as Optional Dependency

Add `ggml-sys` to the `[dependencies]` section:

```toml
[dependencies]
# ... existing dependencies ...
ggml-sys = { git = "https://github.com/your-username/ggml-sys.git", branch = "main", optional = true }
# OR if using path dependency:
# ggml-sys = { path = "../../ggml-sys", optional = true }
```

### 2.2 Add `use-shared-ggml` Feature

Add the feature to the `[features]` section:

```toml
[features]
# ... existing features ...
use-shared-ggml = ["ggml-sys"]
```

## Step 3: Modify `llama-cpp-sys-2/build.rs`

### 3.1 Add Code to Get GGML Paths from `ggml-sys`

At the top of the `main()` function, after getting the manifest directory:

```rust
// Get ggml-sys paths if available (when use-shared-ggml is enabled)
let ggml_lib_dir = env::var("DEP_GGML_SYS_ROOT")
    .map(|root| PathBuf::from(root).join("lib"))
    .ok();
let ggml_include_dir = env::var("DEP_GGML_SYS_INCLUDE")
    .map(PathBuf::from)
    .ok();
let ggml_prefix = ggml_lib_dir.as_ref()
    .and_then(|lib_dir| lib_dir.parent().map(|p| p.to_path_buf()));
```

### 3.2 Modify the Build Logic to Handle `use-shared-ggml`

Find the section where CMake is configured (usually where `Config::new` is called) and modify it:

```rust
// If use-shared-ggml feature is enabled, use system ggml (shared library)
if cfg!(feature = "use-shared-ggml") {
    // Tell CMake to use system ggml instead of building it
    config.define("GGML_USE_SYSTEM_GGML", "ON");
    
    // CRITICAL: Tell CMake where to find ggml
    if let Some(ref prefix) = ggml_prefix {
        // Set CMAKE_PREFIX_PATH to where ggml-sys installed ggml
        config.define("CMAKE_PREFIX_PATH", prefix.to_str().unwrap());
        // Set ggml_DIR to the cmake config directory
        let ggml_cmake_dir = prefix.join("lib").join("cmake").join("ggml");
        if ggml_cmake_dir.exists() {
            config.define("ggml_DIR", ggml_cmake_dir.to_str().unwrap());
        }
    }
    
    // Alternative: If CMake config files aren't in the expected location,
    // you may need to set additional paths
    if let Some(ref lib_dir) = ggml_lib_dir {
        println!("cargo:rustc-link-search=native={}", lib_dir.display());
    }
    if let Some(ref include_dir) = ggml_include_dir {
        // Add include directory for CMake
        config.define("GGML_INCLUDE_DIR", include_dir.to_str().unwrap());
    }
    
    // Link to shared ggml libraries from ggml-sys
    println!("cargo:rustc-link-lib=dylib=ggml");
    println!("cargo:rustc-link-lib=dylib=ggml-base");
    println!("cargo:rustc-link-lib=dylib=ggml-cpu");
    
    if cfg!(target_os = "macos") || cfg!(feature = "openblas") {
        println!("cargo:rustc-link-lib=dylib=ggml-blas");
    }
    
    if cfg!(feature = "vulkan") {
        println!("cargo:rustc-link-lib=dylib=ggml-vulkan");
    }
    
    if cfg!(feature = "hipblas") {
        println!("cargo:rustc-link-lib=dylib=ggml-hip");
    }
    
    if cfg!(feature = "metal") {
        println!("cargo:rustc-link-lib=dylib=ggml-metal");
    }
    
    if cfg!(feature = "cuda") {
        println!("cargo:rustc-link-lib=dylib=ggml-cuda");
    }
    
    if cfg!(feature = "openblas") {
        println!("cargo:rustc-link-lib=dylib=ggml-blas");
    }
    
    if cfg!(feature = "intel-sycl") {
        println!("cargo:rustc-link-lib=dylib=ggml-sycl");
    }
} else {
    // Original code: build llama.cpp with embedded ggml
    // ... existing build logic ...
}
```

### 3.3 Filter Out GGML Libraries from Link List

Find where libraries are collected and filtered. When `use-shared-ggml` is enabled, filter out GGML libraries since they're already linked:

```rust
let llama_libs: Vec<String> = if cfg!(feature = "use-shared-ggml") {
    // Filter out ggml libraries - they're already linked from ggml-sys
    llama_libs
        .into_iter()
        .filter(|lib| !lib.starts_with("ggml"))
        .collect()
} else {
    // Original code: include all libraries including embedded ggml
    llama_libs
};
```

### 3.4 Update Bindgen Includes (if needed)

If bindgen is used to generate bindings, update it to use `ggml-sys` headers when `use-shared-ggml` is enabled:

```rust
let mut builder = bindgen::Builder::default()
    .header("wrapper.h");

// When use-shared-ggml is enabled, use ggml-sys headers
if cfg!(feature = "use-shared-ggml") {
    if let Some(ref include_dir) = ggml_include_dir {
        builder = builder.clang_arg(format!("-I{}", include_dir.display()));
    }
} else {
    // Use embedded ggml headers
    builder = builder.clang_arg(format!("-I{}", llama_src.join("ggml/include").display()));
}

let bindings = builder
    .clang_arg(format!("-I{}", llama_src.display()))
    .clang_arg(format!("-I{}", llama_src.join("include").display()))
    .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
    .generate();
```

## Step 4: Update `wrapper.h` (if needed)

Ensure `wrapper.h` includes the correct path:

```c
#include "llama.h"  // Not <llama.cpp/include/llama.h>
```

## Step 5: Handle CMake Configuration

The `llama.cpp` CMakeLists.txt may need to be configured to find the system GGML. Check if it supports `GGML_USE_SYSTEM_GGML`. If not, you may need to:

1. Set `CMAKE_PREFIX_PATH` to where `ggml-sys` installed GGML
2. Set `ggml_DIR` to the CMake config directory
3. Manually set include and library paths

Example CMake configuration:

```rust
if cfg!(feature = "use-shared-ggml") {
    config.define("GGML_USE_SYSTEM_GGML", "ON");
    
    if let Some(ref prefix) = ggml_prefix {
        config.define("CMAKE_PREFIX_PATH", prefix.to_str().unwrap());
        let ggml_cmake_dir = prefix.join("lib").join("cmake").join("ggml");
        if ggml_cmake_dir.exists() {
            config.define("ggml_DIR", ggml_cmake_dir.to_str().unwrap());
        }
    }
    
    // Fallback: Set paths directly if CMake config isn't found
    if let Some(ref lib_dir) = ggml_lib_dir {
        config.define("GGML_LIB_DIR", lib_dir.to_str().unwrap());
    }
    if let Some(ref include_dir) = ggml_include_dir {
        config.define("GGML_INCLUDE_DIR", include_dir.to_str().unwrap());
    }
}
```

## Step 6: Test the Build

1. Build with the feature enabled:
   ```bash
   cargo build --features use-shared-ggml
   ```

2. Verify that:
   - No GGML source is compiled (only llama.cpp)
   - Links to `dylib=ggml` libraries
   - CMake finds the shared GGML library
   - No duplicate symbol errors

## Step 7: Update Documentation

Update your crate's README to document the new feature:

```markdown
## Features

- `use-shared-ggml`: Use a shared GGML library from `ggml-sys` instead of building embedded GGML.
  This is useful when using both `llama-cpp-2` and `whisper-rs` together to avoid duplicate symbol conflicts.

  ```toml
  [dependencies]
  llama-cpp-2 = { git = "...", features = ["use-shared-ggml"] }
  ggml-sys = { git = "..." }
  ```
```

## Critical Points

1. **`GGML_USE_SYSTEM_GGML=ON`**: Must be set when using shared GGML (if supported by llama.cpp CMakeLists.txt)
2. **`CMAKE_PREFIX_PATH`**: Must point to where `ggml-sys` installed GGML
3. **`ggml_DIR`**: Should point to the CMake config directory if available
4. **Link to `dylib=ggml`**: Not static libraries
5. **Filter GGML libraries**: Remove GGML libraries from the link list since they're already linked
6. **Export paths**: Use `DEP_GGML_SYS_ROOT` and `DEP_GGML_SYS_INCLUDE` from `ggml-sys`

## Verification Checklist

- [ ] `use-shared-ggml` feature is defined in both `llama-cpp-2` and `llama-cpp-sys-2` Cargo.toml
- [ ] `ggml-sys` is added as optional dependency
- [ ] Build script checks for `use-shared-ggml` feature
- [ ] `GGML_USE_SYSTEM_GGML=ON` is set when using shared GGML (if supported)
- [ ] `CMAKE_PREFIX_PATH` or `ggml_DIR` points to where `ggml-sys` installed GGML
- [ ] Links to `dylib=ggml` libraries (not static)
- [ ] GGML libraries are filtered out from llama's library list
- [ ] Build succeeds with `--features use-shared-ggml`

## Troubleshooting

### CMake can't find ggml package

**Error**: `Could not find a package configuration file provided by "ggml"`

**Solution**: 
- Ensure `CMAKE_PREFIX_PATH` is set correctly
- Check that `ggml-sys` exports `DEP_GGML_SYS_ROOT`
- Verify that CMake config files exist in `$DEP_GGML_SYS_ROOT/lib/cmake/ggml/`
- If CMake config files don't exist, set `GGML_LIB_DIR` and `GGML_INCLUDE_DIR` directly

### Duplicate symbol errors

**Error**: Multiple definition of `ggml_*` symbols

**Solution**:
- Ensure you're linking to `dylib=ggml`, not `static=ggml`
- Verify that `ggml-sys` has `links = "ggml"` in its Cargo.toml
- Check that GGML libraries are filtered out from the link list
- Ensure only one crate is building GGML

### Build fails with missing headers

**Error**: `fatal error: 'ggml.h' file not found`

**Solution**:
- Ensure `ggml_include_dir` is set correctly
- Check that `DEP_GGML_SYS_INCLUDE` is exported by `ggml-sys`
- Verify bindgen includes the correct paths

### llama.cpp CMakeLists.txt doesn't support GGML_USE_SYSTEM_GGML

**Error**: CMake variable `GGML_USE_SYSTEM_GGML` is not recognized

**Solution**:
- Check if `llama.cpp` CMakeLists.txt supports this option
- If not, you may need to:
  1. Manually set include and library paths
  2. Modify the CMakeLists.txt to support system GGML
  3. Use a different approach (e.g., set `CMAKE_PREFIX_PATH` and let CMake find it)

### Build still compiles GGML

**Error**: GGML source files are still being compiled

**Solution**:
- Verify that `GGML_USE_SYSTEM_GGML=ON` is set
- Check that CMake is finding the system GGML
- Ensure the CMakeLists.txt respects the `GGML_USE_SYSTEM_GGML` flag
- Check CMake output to see if it's using system GGML or building embedded GGML

