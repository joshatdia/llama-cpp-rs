use cmake::Config;
use glob::glob;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::DirEntry;

enum WindowsVariant {
    Msvc,
    Other,
}

enum AppleVariant {
    MacOS,
    Other,
}

enum TargetOs {
    Windows(WindowsVariant),
    Apple(AppleVariant),
    Linux,
    Android,
}

macro_rules! debug_log {
    ($($arg:tt)*) => {
        if std::env::var("BUILD_DEBUG").is_ok() {
            println!("cargo:warning=[DEBUG] {}", format!($($arg)*));
        }
    };
}

fn parse_target_os() -> Result<(TargetOs, String), String> {
    let target = env::var("TARGET").unwrap();

    if target.contains("windows") {
        if target.ends_with("-windows-msvc") {
            Ok((TargetOs::Windows(WindowsVariant::Msvc), target))
        } else {
            Ok((TargetOs::Windows(WindowsVariant::Other), target))
        }
    } else if target.contains("apple") {
        if target.ends_with("-apple-darwin") {
            Ok((TargetOs::Apple(AppleVariant::MacOS), target))
        } else {
            Ok((TargetOs::Apple(AppleVariant::Other), target))
        }
    } else if target.contains("android")
        || target == "aarch64-linux-android"
        || target == "armv7-linux-androideabi"
        || target == "i686-linux-android"
        || target == "x86_64-linux-android"
    {
        // Handle both full android targets and short names like arm64-v8a that cargo ndk might use
        Ok((TargetOs::Android, target))
    } else if target.contains("linux") {
        Ok((TargetOs::Linux, target))
    } else {
        Err(target)
    }
}

fn get_cargo_target_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let out_dir = env::var("OUT_DIR")?;
    let path = PathBuf::from(out_dir);
    let target_dir = path
        .ancestors()
        .nth(3)
        .ok_or("OUT_DIR is not deep enough")?;
    Ok(target_dir.to_path_buf())
}

fn extract_lib_names(out_dir: &Path, build_shared_libs: bool) -> Vec<String> {
    let lib_pattern = if cfg!(windows) {
        "*.lib"
    } else if cfg!(target_os = "macos") {
        if build_shared_libs {
            "*.dylib"
        } else {
            "*.a"
        }
    } else if build_shared_libs {
        "*.so"
    } else {
        "*.a"
    };
    let libs_dir = out_dir.join("lib*");
    let pattern = libs_dir.join(lib_pattern);
    debug_log!("Extract libs {}", pattern.display());

    let mut lib_names: Vec<String> = Vec::new();

    // Process the libraries based on the pattern
    for entry in glob(pattern.to_str().unwrap()).unwrap() {
        match entry {
            Ok(path) => {
                let stem = path.file_stem().unwrap();
                let stem_str = stem.to_str().unwrap();

                // Remove the "lib" prefix if present
                let lib_name = if stem_str.starts_with("lib") {
                    stem_str.strip_prefix("lib").unwrap_or(stem_str)
                } else {
                    if path.extension() == Some(std::ffi::OsStr::new("a")) {
                        let target = path.parent().unwrap().join(format!("lib{}.a", stem_str));
                        std::fs::rename(&path, &target).unwrap_or_else(|e| {
                            panic!("Failed to rename {path:?} to {target:?}: {e:?}");
                        })
                    }
                    stem_str
                };
                lib_names.push(lib_name.to_string());
            }
            Err(e) => println!("cargo:warning=error={}", e),
        }
    }
    lib_names
}

fn extract_lib_assets(out_dir: &Path) -> Vec<PathBuf> {
    let shared_lib_pattern = if cfg!(windows) {
        "*.dll"
    } else if cfg!(target_os = "macos") {
        "*.dylib"
    } else {
        "*.so"
    };

    let shared_libs_dir = if cfg!(windows) { "bin" } else { "lib" };
    let libs_dir = out_dir.join(shared_libs_dir);
    let pattern = libs_dir.join(shared_lib_pattern);
    debug_log!("Extract lib assets {}", pattern.display());
    let mut files = Vec::new();

    for entry in glob(pattern.to_str().unwrap()).unwrap() {
        match entry {
            Ok(path) => {
                files.push(path);
            }
            Err(e) => eprintln!("cargo:warning=error={}", e),
        }
    }

    files
}

fn macos_link_search_path() -> Option<String> {
    let output = Command::new("clang")
        .arg("--print-search-dirs")
        .output()
        .ok()?;
    if !output.status.success() {
        println!(
            "failed to run 'clang --print-search-dirs', continuing without a link search path"
        );
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if line.contains("libraries: =") {
            let path = line.split('=').nth(1)?;
            return Some(format!("{}/lib/darwin", path));
        }
    }

    println!("failed to determine link search path, continuing without it");
    None
}

fn validate_android_ndk(ndk_path: &str) -> Result<(), String> {
    let ndk_path = Path::new(ndk_path);

    if !ndk_path.exists() {
        return Err(format!(
            "Android NDK path does not exist: {}",
            ndk_path.display()
        ));
    }

    let toolchain_file = ndk_path.join("build/cmake/android.toolchain.cmake");
    if !toolchain_file.exists() {
        return Err(format!(
            "Android NDK toolchain file not found: {}\n\
             This indicates an incomplete NDK installation.",
            toolchain_file.display()
        ));
    }

    Ok(())
}

fn is_hidden(e: &DirEntry) -> bool {
    e.file_name()
        .to_str()
        .map(|s| s.starts_with('.'))
        .unwrap_or_default()
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let (target_os, target_triple) =
        parse_target_os().unwrap_or_else(|t| panic!("Failed to parse target os {t}"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    let target_dir = get_cargo_target_dir().unwrap();
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("Failed to get CARGO_MANIFEST_DIR");
    let llama_src = Path::new(&manifest_dir).join("llama.cpp");
    let build_shared_libs = cfg!(feature = "dynamic-link");

    // Determine namespace based on features (for use-shared-ggml)
    // This determines the library name prefix when using namespaced GGML
    let ggml_namespace = if cfg!(feature = "namespace-llama") {
        Some("ggml_llama")
    } else if cfg!(feature = "namespace-whisper") {
        Some("ggml_whisper")
    } else {
        None  // Default: no namespace (for backward compatibility)
    };

    if let Some(ns) = ggml_namespace {
        println!("cargo:warning=[GGML] Using namespaced GGML libraries: {}", ns);
        debug_log!("Using GGML namespace: {}", ns);
    } else if cfg!(feature = "use-shared-ggml") {
        println!("cargo:warning=[GGML] No namespace specified - using default GGML symbols");
        println!("cargo:warning=[GGML] WARNING: If using with both llama.cpp and whisper.cpp, enable namespace-llama or namespace-whisper");
        debug_log!("No namespace specified - using default GGML symbols");
        debug_log!("WARNING: If using with both llama.cpp and whisper.cpp, enable namespace-llama or namespace-whisper");
    }

    // Get ggml-rs paths if available (when use-shared-ggml is enabled)
    // Try both DEP_GGML_* and DEP_GGML_RS_* variable names for compatibility
    // Note: The actual crate name is "ggml", so it exports DEP_GGML_* variables
    let ggml_root = env::var("DEP_GGML_ROOT")
        .or_else(|_| env::var("DEP_GGML_RS_ROOT"))
        .or_else(|_| {
            // Fallback: try to derive root from DEP_GGML_LIB_DIR
            env::var("DEP_GGML_LIB_DIR")
                .or_else(|_| env::var("DEP_GGML_RS_LIB_DIR"))
                .map(|lib| {
                    let lib_path = PathBuf::from(&lib);
                    lib_path
                        .parent()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|| lib)
                })
        })
        .ok();
    
    let ggml_lib_dir = env::var("DEP_GGML_LIB_DIR")
        .or_else(|_| env::var("DEP_GGML_RS_LIB_DIR"))
        .map(PathBuf::from)
        .ok()
        .or_else(|| {
            ggml_root.as_ref()
                .map(|root| PathBuf::from(root).join("lib"))
        });
    let ggml_include_dir = env::var("DEP_GGML_INCLUDE")
        .or_else(|_| env::var("DEP_GGML_RS_INCLUDE"))
        .map(PathBuf::from)
        .ok();
    let ggml_prefix = ggml_root.as_ref()
        .map(|root| PathBuf::from(root));

    let build_shared_libs = std::env::var("LLAMA_BUILD_SHARED_LIBS")
        .map(|v| v == "1")
        .unwrap_or(build_shared_libs);
    let profile = env::var("LLAMA_LIB_PROFILE").unwrap_or("Release".to_string());
    let static_crt = env::var("LLAMA_STATIC_CRT")
        .map(|v| v == "1")
        .unwrap_or(false);

    println!("cargo:rerun-if-env-changed=LLAMA_LIB_PROFILE");
    println!("cargo:rerun-if-env-changed=LLAMA_BUILD_SHARED_LIBS");
    println!("cargo:rerun-if-env-changed=LLAMA_STATIC_CRT");

    debug_log!("TARGET: {}", target_triple);
    debug_log!("CARGO_MANIFEST_DIR: {}", manifest_dir);
    debug_log!("TARGET_DIR: {}", target_dir.display());
    debug_log!("OUT_DIR: {}", out_dir.display());
    debug_log!("BUILD_SHARED: {}", build_shared_libs);

    // Make sure that changes to the llama.cpp project trigger a rebuild.
    let rebuild_on_children_of = [
        llama_src.join("src"),
        llama_src.join("ggml/src"),
        llama_src.join("common"),
    ];
    for entry in walkdir::WalkDir::new(&llama_src)
        .into_iter()
        .filter_entry(|e| !is_hidden(e))
    {
        let entry = entry.expect("Failed to obtain entry");
        let rebuild = entry
            .file_name()
            .to_str()
            .map(|f| f.starts_with("CMake"))
            .unwrap_or_default()
            || rebuild_on_children_of
                .iter()
                .any(|src_folder| entry.path().starts_with(src_folder));
        if rebuild {
            println!("cargo:rerun-if-changed={}", entry.path().display());
        }
    }

    // Speed up build
    env::set_var(
        "CMAKE_BUILD_PARALLEL_LEVEL",
        std::thread::available_parallelism()
            .unwrap()
            .get()
            .to_string(),
    );

    // Bindings
    let mut bindings_builder = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg(format!("-I{}", llama_src.join("include").display()))
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .derive_partialeq(true)
        .allowlist_function("ggml_.*")
        .allowlist_type("ggml_.*")
        .allowlist_function("llama_.*")
        .allowlist_type("llama_.*")
        .prepend_enum_name(false);

    // When use-shared-ggml is enabled, use ggml-rs headers instead of embedded ggml
    if cfg!(feature = "use-shared-ggml") {
        debug_log!("use-shared-ggml feature is enabled");
        
        // Debug: List all DEP_* environment variables to help diagnose
        debug_log!("Available DEP_* environment variables:");
        for (key, value) in env::vars() {
            if key.starts_with("DEP_") {
                debug_log!("  {} = {}", key, value);
            }
        }
        
        debug_log!("DEP_GGML_INCLUDE: {:?}", env::var("DEP_GGML_INCLUDE"));
        debug_log!("DEP_GGML_ROOT: {:?}", env::var("DEP_GGML_ROOT"));
        debug_log!("DEP_GGML_LIB_DIR: {:?}", env::var("DEP_GGML_LIB_DIR"));
        debug_log!("ggml_include_dir: {:?}", ggml_include_dir);
        debug_log!("ggml_lib_dir: {:?}", ggml_lib_dir);
        
        if let Some(ref include_dir) = ggml_include_dir {
            debug_log!("Using ggml-rs include directory: {}", include_dir.display());
            bindings_builder = bindings_builder.clang_arg(format!("-I{}", include_dir.display()));
        } else {
            // Fallback: try to find it from DEP_GGML_ROOT
            if let Ok(root) = env::var("DEP_GGML_ROOT") {
                let include_path = PathBuf::from(root).join("include");
                debug_log!("Trying fallback include path: {}", include_path.display());
                if include_path.exists() {
                    debug_log!("Using fallback include directory: {}", include_path.display());
                    bindings_builder = bindings_builder.clang_arg(format!("-I{}", include_path.display()));
                } else {
                    panic!("use-shared-ggml feature is enabled but cannot find ggml-rs headers. DEP_GGML_INCLUDE or DEP_GGML_ROOT/include must be set. Tried: {}", include_path.display());
                }
            } else {
                // List all available DEP_* variables to help diagnose
                let mut dep_vars = Vec::new();
                for (key, value) in env::vars() {
                    if key.starts_with("DEP_") {
                        dep_vars.push(format!("  {} = {}", key, value));
                    }
                }
                
                let dep_vars_str = if dep_vars.is_empty() {
                    "  (none found)".to_string()
                } else {
                    dep_vars.join("\n")
                };
                
                panic!(
                    "use-shared-ggml feature is enabled but DEP_GGML_ROOT is not set.\n\
                     Make sure ggml-rs is properly configured and added to [build-dependencies] in Cargo.toml.\n\
                     \n\
                     Available DEP_* environment variables:\n\
                     {}\n\
                     \n\
                     Note: The ggml-rs crate exports DEP_GGML_* variables (not DEP_GGML_RS_*).",
                    dep_vars_str
                );
            }
        }
    } else {
        // Use embedded ggml headers
        debug_log!("Using embedded ggml headers");
        bindings_builder = bindings_builder.clang_arg(format!("-I{}", llama_src.join("ggml/include").display()));
    }

    // Configure mtmd feature if enabled
    if cfg!(feature = "mtmd") {
        bindings_builder = bindings_builder
            .header("wrapper_mtmd.h")
            .allowlist_function("mtmd_.*")
            .allowlist_type("mtmd_.*");
    }

    // Configure Android-specific bindgen settings
    if matches!(target_os, TargetOs::Android) {
        // Detect Android NDK from environment variables
        let android_ndk = env::var("ANDROID_NDK")
            .or_else(|_| env::var("ANDROID_NDK_ROOT"))
            .or_else(|_| env::var("NDK_ROOT"))
            .or_else(|_| env::var("CARGO_NDK_ANDROID_NDK"))
            .or_else(|_| {
                // Try to auto-detect NDK from Android SDK
                if let Some(home) = env::home_dir() {
                    let android_home = env::var("ANDROID_HOME")
                        .or_else(|_| env::var("ANDROID_SDK_ROOT"))
                        .unwrap_or_else(|_| format!("{}/Android/Sdk", home.display()));

                    let ndk_dir = format!("{}/ndk", android_home);
                    if let Ok(entries) = std::fs::read_dir(&ndk_dir) {
                        let mut versions: Vec<_> = entries
                            .filter_map(|e| e.ok())
                            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                            .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
                            .collect();
                        versions.sort();
                        if let Some(latest) = versions.last() {
                            return Ok(format!("{}/{}", ndk_dir, latest));
                        }
                    }
                }
                Err(env::VarError::NotPresent)
            })
            .unwrap_or_else(|_| {
                panic!(
                    "Android NDK not found. Please set one of: ANDROID_NDK, NDK_ROOT, ANDROID_NDK_ROOT\n\
                     Current target: {}\n\
                     Download from: https://developer.android.com/ndk/downloads",
                    target_triple
                );
            });

        // Get Android API level
        let android_api = env::var("ANDROID_API_LEVEL")
            .or_else(|_| env::var("ANDROID_PLATFORM").map(|p| p.replace("android-", "")))
            .or_else(|_| env::var("CARGO_NDK_ANDROID_PLATFORM").map(|p| p.replace("android-", "")))
            .unwrap_or_else(|_| "28".to_string());

        // Determine host platform
        let host_tag = if cfg!(target_os = "macos") {
            "darwin-x86_64"
        } else if cfg!(target_os = "linux") {
            "linux-x86_64"
        } else if cfg!(target_os = "windows") {
            "windows-x86_64"
        } else {
            panic!("Unsupported host platform for Android NDK");
        };

        // Map Rust target to Android architecture
        let android_target_prefix = if target_triple.contains("aarch64") {
            "aarch64-linux-android"
        } else if target_triple.contains("armv7") {
            "arm-linux-androideabi"
        } else if target_triple.contains("x86_64") {
            "x86_64-linux-android"
        } else if target_triple.contains("i686") {
            "i686-linux-android"
        } else {
            panic!("Unsupported Android target: {}", target_triple);
        };

        // Setup Android toolchain paths
        let toolchain_path = format!("{}/toolchains/llvm/prebuilt/{}", android_ndk, host_tag);
        let sysroot = format!("{}/sysroot", toolchain_path);

        // Validate toolchain existence
        if !std::path::Path::new(&toolchain_path).exists() {
            panic!(
                "Android NDK toolchain not found at: {}\n\
                 Please ensure you have the correct Android NDK for your platform.",
                toolchain_path
            );
        }

        // Find clang builtin includes
        let clang_builtin_includes = {
            let clang_lib_path = format!("{}/lib/clang", toolchain_path);
            std::fs::read_dir(&clang_lib_path).ok().and_then(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .find(|entry| {
                        entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
                            && entry
                                .file_name()
                                .to_str()
                                .map(|name| name.chars().next().unwrap_or('0').is_ascii_digit())
                                .unwrap_or(false)
                    })
                    .and_then(|entry| {
                        let include_path =
                            format!("{}/{}/include", clang_lib_path, entry.file_name().to_str()?);
                        if std::path::Path::new(&include_path).exists() {
                            Some(include_path)
                        } else {
                            None
                        }
                    })
            })
        };

        // Configure bindgen for Android
        bindings_builder = bindings_builder
            .clang_arg(format!("--sysroot={}", sysroot))
            .clang_arg(format!("-D__ANDROID_API__={}", android_api))
            .clang_arg("-D__ANDROID__");

        // Add include paths in correct order
        if let Some(ref builtin_includes) = clang_builtin_includes {
            bindings_builder = bindings_builder
                .clang_arg("-isystem")
                .clang_arg(builtin_includes);
        }

        bindings_builder = bindings_builder
            .clang_arg("-isystem")
            .clang_arg(format!("{}/usr/include/{}", sysroot, android_target_prefix))
            .clang_arg("-isystem")
            .clang_arg(format!("{}/usr/include", sysroot))
            .clang_arg("-include")
            .clang_arg("stdbool.h")
            .clang_arg("-include")
            .clang_arg("stdint.h");

        // Set additional clang args for cargo ndk compatibility
        if env::var("CARGO_SUBCOMMAND").as_deref() == Ok("ndk") {
            std::env::set_var(
                "BINDGEN_EXTRA_CLANG_ARGS",
                format!("--target={}", target_triple),
            );
        }
    }

    // Fix bindgen header discovery on Windows MSVC
    // Use cc crate to discover MSVC include paths by compiling a dummy file
    if matches!(target_os, TargetOs::Windows(WindowsVariant::Msvc)) {
        // Create a minimal dummy C file to extract compiler flags
        let out_dir = env::var("OUT_DIR").unwrap();
        let dummy_c = Path::new(&out_dir).join("dummy.c");
        std::fs::write(&dummy_c, "int main() { return 0; }").unwrap();

        // Use cc crate to get compiler with proper environment setup
        let mut build = cc::Build::new();
        build.file(&dummy_c);

        // Get the actual compiler command cc would use
        let compiler = build.try_get_compiler().unwrap();

        // Extract include paths by checking compiler's environment
        // cc crate sets up MSVC environment internally
        let env_include = compiler
            .env()
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("INCLUDE"))
            .map(|(_, v)| v);

        if let Some(include_paths) = env_include {
            for include_path in include_paths
                .to_string_lossy()
                .split(';')
                .filter(|s| !s.is_empty())
            {
                bindings_builder = bindings_builder
                    .clang_arg("-isystem")
                    .clang_arg(include_path);
                debug_log!("Added MSVC include path: {}", include_path);
            }
        }

        // Add MSVC compatibility flags
        bindings_builder = bindings_builder
            .clang_arg(format!("--target={}", target_triple))
            .clang_arg("-fms-compatibility")
            .clang_arg("-fms-extensions");

        debug_log!(
            "Configured bindgen with MSVC toolchain for target: {}",
            target_triple
        );
    }
    let bindings = bindings_builder
        .generate()
        .expect("Failed to generate bindings");

    // Write the generated bindings to an output file
    let bindings_path = out_dir.join("bindings.rs");
    bindings
        .write_to_file(bindings_path)
        .expect("Failed to write bindings");

    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-changed=wrapper_mtmd.h");

    debug_log!("Bindings Created");

    // Build with Cmake

    let mut config = Config::new(&llama_src);

    // If use-shared-ggml feature is enabled, use system ggml (shared library)
    if cfg!(feature = "use-shared-ggml") {
        // Tell CMake to use system ggml instead of building it
        config.define("LLAMA_USE_SYSTEM_GGML", "ON");
        
        // Determine library base name based on namespace
        let lib_base_name = ggml_namespace.unwrap_or("ggml");
        
        // CRITICAL: Tell CMake where to find ggml
        // Since we're using namespaced libraries, we need to manually set the library path
        // instead of relying on ggml-config.cmake which looks for "ggml" instead of "ggml_llama"
        if let Some(ref prefix) = ggml_prefix {
            // Set CMAKE_PREFIX_PATH to where ggml-rs installed ggml
            config.define("CMAKE_PREFIX_PATH", prefix.to_str().unwrap());
            
            // Try to use ggml-config.cmake if it exists, but we'll patch it for namespaced libraries
            let ggml_cmake_dir = prefix.join("lib").join("cmake").join("ggml");
            if ggml_cmake_dir.exists() {
                config.define("ggml_DIR", ggml_cmake_dir.to_str().unwrap());
                
                // Patch ggml-config.cmake to use namespaced library names
                // This is necessary because ggml-config.cmake looks for "ggml", "ggml-base", etc.
                // but we have "ggml_llama", "ggml_llama-base", etc.
                if let Some(ref namespace) = ggml_namespace {
                    let ggml_config_path = ggml_cmake_dir.join("ggml-config.cmake");
                    if ggml_config_path.exists() {
                        match std::fs::read_to_string(&ggml_config_path) {
                            Ok(config_content) => {
                                // Replace all library name references with namespaced versions
                                // The pattern in ggml-config.cmake is:
                                //   find_library(GGML_LIBRARY ggml
                                //   find_library(GGML_BASE_LIBRARY ggml-base
                                //   find_library(${_ggml_backend_pfx}_LIBRARY ${_ggml_backend}
                                // where ${_ggml_backend} can be ggml-cpu, ggml-cuda, etc.
                                // Replace all library name references with namespaced versions
                                // Order matters: replace specific patterns first, then general ones
                                let mut patched = config_content.clone();
                                
                                // Replace specific find_library calls first (most specific patterns)
                                patched = patched.replace(
                                    "find_library(GGML_BASE_LIBRARY ggml-base",
                                    &format!("find_library(GGML_BASE_LIBRARY {}-base", namespace)
                                );
                                patched = patched.replace(
                                    "find_library(GGML_LIBRARY ggml",
                                    &format!("find_library(GGML_LIBRARY {}", namespace)
                                );
                                
                                // Replace component library names in backend lists and variables
                                // These appear in GGML_AVAILABLE_BACKENDS and ${_ggml_backend} usage
                                patched = patched.replace("ggml-cpu", &format!("{}-cpu", namespace));
                                patched = patched.replace("ggml-cuda", &format!("{}-cuda", namespace));
                                patched = patched.replace("ggml-vulkan", &format!("{}-vulkan", namespace));
                                patched = patched.replace("ggml-metal", &format!("{}-metal", namespace));
                                
                                // Replace quoted library names (for safety)
                                patched = patched.replace("\"ggml\"", &format!("\"{}\"", namespace));
                                patched = patched.replace("'ggml'", &format!("'{}'", namespace));
                                patched = patched.replace("\"ggml-base\"", &format!("\"{}-base\"", namespace));
                                patched = patched.replace("'ggml-base'", &format!("'{}-base'", namespace));
                                
                                if patched != config_content {
                                    if let Err(e) = std::fs::write(&ggml_config_path, &patched) {
                                        eprintln!(
                                            "cargo:warning=[GGML] Failed to patch ggml-config.cmake: {}\n\
                                             CMake may fail to find namespaced libraries.",
                                            e
                                        );
                                    } else {
                                        println!("cargo:warning=[GGML] Patched ggml-config.cmake to use namespaced library: {}", namespace);
                                        debug_log!("Patched ggml-config.cmake: {} -> {}", "ggml", namespace);
                                    }
                                }
                            }
                            Err(e) => {
                                eprintln!(
                                    "cargo:warning=[GGML] Could not read ggml-config.cmake to patch: {}\n\
                                     CMake may fail to find namespaced libraries.",
                                    e
                                );
                            }
                        }
                    }
                }
            }
        }
        
        // Set library search path
        if let Some(ref lib_dir) = ggml_lib_dir {
            println!("cargo:rustc-link-search=native={}", lib_dir.display());
            debug_log!("[GGML] Library search path: {}", lib_dir.display());
            if lib_dir.exists() {
                debug_log!("[GGML] Library directory exists");
                
                // Manually set the namespaced library path for CMake
                // This overrides what ggml-config.cmake looks for
                let ggml_lib_name = if cfg!(windows) {
                    format!("{}.lib", lib_base_name)
                } else if cfg!(target_os = "macos") {
                    format!("lib{}.dylib", lib_base_name)
                } else {
                    format!("lib{}.so", lib_base_name)
                };
                let ggml_lib_path = lib_dir.join(&ggml_lib_name);
                
                if ggml_lib_path.exists() {
                    // Set the library path directly for CMake as a cache variable
                    // This will be used by find_library in ggml-config.cmake (after patching)
                    config.define("GGML_LIBRARY", ggml_lib_path.to_str().unwrap());
                    // Also set it as a cache variable so find_library will use it
                    config.define("GGML_LIBRARY:FILEPATH", ggml_lib_path.to_str().unwrap());
                    println!("cargo:warning=[GGML] Setting GGML_LIBRARY to: {}", ggml_lib_path.display());
                    
                    // Also create fallback libraries for component libraries if using namespace
                    // This is a backup in case the patching doesn't work completely
                    if ggml_namespace.is_some() {
                        let component_libs = vec!["base", "cpu"];
                        let mut feature_components = Vec::new();
                        if cfg!(feature = "cuda") {
                            feature_components.push("cuda");
                        }
                        if cfg!(feature = "vulkan") {
                            feature_components.push("vulkan");
                        }
                        if cfg!(feature = "metal") {
                            feature_components.push("metal");
                        }
                        
                        for component in component_libs.iter().chain(feature_components.iter()) {
                            let namespaced_lib_name = if cfg!(windows) {
                                format!("{}-{}.lib", lib_base_name, component)
                            } else if cfg!(target_os = "macos") {
                                format!("lib{}-{}.dylib", lib_base_name, component)
                            } else {
                                format!("lib{}-{}.so", lib_base_name, component)
                            };
                            let namespaced_lib_path = lib_dir.join(&namespaced_lib_name);
                            
                            if namespaced_lib_path.exists() {
                                let fallback_lib_name = if cfg!(windows) {
                                    format!("ggml-{}.lib", component)
                                } else if cfg!(target_os = "macos") {
                                    format!("libggml-{}.dylib", component)
                                } else {
                                    format!("libggml-{}.so", component)
                                };
                                let fallback_lib_path = lib_dir.join(&fallback_lib_name);
                                
                                if !fallback_lib_path.exists() {
                                    // Try to create a hard link (works on Windows and Unix)
                                    if let Err(e) = std::fs::hard_link(&namespaced_lib_path, &fallback_lib_path) {
                                        // If hard link fails, try copy (for cross-filesystem scenarios)
                                        if let Err(e2) = std::fs::copy(&namespaced_lib_path, &fallback_lib_path) {
                                            debug_log!("Could not create fallback library {}: {} / {}", fallback_lib_name, e, e2);
                                        } else {
                                            debug_log!("Created fallback library: {} -> {}", fallback_lib_name, namespaced_lib_path.display());
                                        }
                                    } else {
                                        debug_log!("Created fallback library link: {} -> {}", fallback_lib_name, namespaced_lib_path.display());
                                    }
                                }
                            }
                        }
                    }
                } else {
                    eprintln!(
                        "cargo:warning=[GGML] Namespaced library {} not found in {}.\n\
                         Make sure ggml-rs is built with the namespace-llama feature.",
                        ggml_lib_name,
                        lib_dir.display()
                    );
                }
            } else {
                eprintln!("cargo:warning=[GGML] Library directory does not exist: {}", lib_dir.display());
            }
        } else {
            eprintln!("cargo:warning=[GGML] Library directory not found. Make sure ggml-rs is built with use-shared-ggml feature.");
        }
        
        // Set include directory for CMake
        if let Some(ref include_dir) = ggml_include_dir {
            config.define("GGML_INCLUDE_DIR", include_dir.to_str().unwrap());
        }
        
        println!("cargo:warning=[GGML] Using namespace-specific GGML libraries with base name: {}", lib_base_name);
        
        // Verify libraries exist (for debugging)
        if let Some(ref lib_dir) = ggml_lib_dir {
            if lib_dir.exists() {
                let base_lib_pattern = if cfg!(windows) {
                    format!("{}.lib", lib_base_name)
                } else if cfg!(target_os = "macos") {
                    format!("lib{}.dylib", lib_base_name)
                } else {
                    format!("lib{}.so", lib_base_name)
                };
                let base_lib = lib_dir.join(&base_lib_pattern);
                
                if !base_lib.exists() {
                    eprintln!(
                        "cargo:warning=[GGML] Base library {} not found in {}.\n\
                         Make sure ggml-rs is built with the namespace-llama feature.\n\
                         In your Cargo.toml, enable the namespace-llama feature on ggml-rs:\n\
                         ggml-rs = {{ version = \"...\", features = [\"namespace-llama\", \"cuda\"] }}",
                        base_lib_pattern,
                        lib_dir.display()
                    );
                    
                    // List available libraries for debugging (make it visible)
                    eprintln!("cargo:warning=[GGML] Available libraries in {}:", lib_dir.display());
                    if let Ok(entries) = std::fs::read_dir(lib_dir) {
                        let mut found_any = false;
                        for entry in entries.flatten() {
                            if let Some(name) = entry.file_name().to_str() {
                                eprintln!("cargo:warning=[GGML]   - {}", name);
                                found_any = true;
                            }
                        }
                        if !found_any {
                            eprintln!("cargo:warning=[GGML]   (no libraries found)");
                        }
                    } else {
                        eprintln!("cargo:warning=[GGML]   (could not read directory)");
                    }
                } else {
                    println!("cargo:warning=[GGML] Found base library: {}", base_lib.display());
                    debug_log!("Found base library: {}", base_lib.display());
                }
            } else {
                eprintln!(
                    "cargo:warning=[GGML] Library directory does not exist: {}.\n\
                     Make sure ggml-rs is built and installed.",
                    lib_dir.display()
                );
            }
        }
        
        // Note: ggml-rs handles linking automatically, so we don't need to link here.
        // We only need to set the library search path (already done above) and copy DLLs (done below).
    }

    // Would require extra source files to pointlessly
    // be included in what's uploaded to and downloaded from
    // crates.io, so deactivating these instead
    config.define("LLAMA_BUILD_TESTS", "OFF");
    config.define("LLAMA_BUILD_EXAMPLES", "OFF");
    config.define("LLAMA_BUILD_SERVER", "OFF");
    config.define("LLAMA_BUILD_TOOLS", "OFF");
    config.define("LLAMA_CURL", "OFF");

    if cfg!(feature = "mtmd") {
        config.define("LLAMA_BUILD_COMMON", "ON");
        // mtmd support in llama-cpp is within the tools directory
        config.define("LLAMA_BUILD_TOOLS", "ON");
    }

    // Pass CMAKE_ environment variables down to CMake
    for (key, value) in env::vars() {
        if key.starts_with("CMAKE_") {
            config.define(&key, &value);
        }
    }

    config.define(
        "BUILD_SHARED_LIBS",
        if build_shared_libs { "ON" } else { "OFF" },
    );

    if matches!(target_os, TargetOs::Apple(_)) {
        config.define("GGML_BLAS", "OFF");
    }

    if (matches!(target_os, TargetOs::Windows(WindowsVariant::Msvc))
        && matches!(
            profile.as_str(),
            "Release" | "RelWithDebInfo" | "MinSizeRel"
        ))
    {
        // Debug Rust builds under MSVC turn off optimization even though we're ideally building the release profile of llama.cpp.
        // Looks like an upstream bug:
        // https://github.com/rust-lang/cmake-rs/issues/240
        // For now explicitly reinject the optimization flags that a CMake Release build is expected to have on in this scenario.
        // This fixes CPU inference performance when part of a Rust debug build.
        for flag in &["/O2", "/DNDEBUG", "/Ob2"] {
            config.cflag(flag);
            config.cxxflag(flag);
        }
    }

    config.static_crt(static_crt);

    if matches!(target_os, TargetOs::Android) {
        // Android NDK Build Configuration
        let android_ndk = env::var("ANDROID_NDK")
            .or_else(|_| env::var("NDK_ROOT"))
            .or_else(|_| env::var("ANDROID_NDK_ROOT"))
            .unwrap_or_else(|_| {
                panic!(
                    "Android NDK not found. Please set one of: ANDROID_NDK, NDK_ROOT, ANDROID_NDK_ROOT\n\
                     Download from: https://developer.android.com/ndk/downloads"
                );
            });

        // Validate NDK installation
        if let Err(error) = validate_android_ndk(&android_ndk) {
            panic!("Android NDK validation failed: {}", error);
        }

        // Rerun build script if NDK environment variables change
        println!("cargo:rerun-if-env-changed=ANDROID_NDK");
        println!("cargo:rerun-if-env-changed=NDK_ROOT");
        println!("cargo:rerun-if-env-changed=ANDROID_NDK_ROOT");

        // Set CMake toolchain file for Android
        let toolchain_file = format!("{}/build/cmake/android.toolchain.cmake", android_ndk);
        config.define("CMAKE_TOOLCHAIN_FILE", &toolchain_file);

        // Configure Android platform (API level)
        let android_platform = env::var("ANDROID_PLATFORM").unwrap_or_else(|_| {
            env::var("ANDROID_API_LEVEL")
                .map(|level| format!("android-{}", level))
                .unwrap_or_else(|_| "android-28".to_string())
        });

        println!("cargo:rerun-if-env-changed=ANDROID_PLATFORM");
        println!("cargo:rerun-if-env-changed=ANDROID_API_LEVEL");
        config.define("ANDROID_PLATFORM", &android_platform);

        // Map Rust target to Android ABI
        let android_abi = if target_triple.contains("aarch64") {
            "arm64-v8a"
        } else if target_triple.contains("armv7") {
            "armeabi-v7a"
        } else if target_triple.contains("x86_64") {
            "x86_64"
        } else if target_triple.contains("i686") {
            "x86"
        } else {
            panic!(
                "Unsupported Android target: {}\n\
                 Supported targets: aarch64-linux-android, armv7-linux-androideabi, i686-linux-android, x86_64-linux-android",
                target_triple
            );
        };

        config.define("ANDROID_ABI", android_abi);

        // Configure architecture-specific compiler flags
        match android_abi {
            "arm64-v8a" => {
                config.cflag("-march=armv8-a");
                config.cxxflag("-march=armv8-a");
            }
            "armeabi-v7a" => {
                config.cflag("-march=armv7-a");
                config.cxxflag("-march=armv7-a");
                config.cflag("-mfpu=neon");
                config.cxxflag("-mfpu=neon");
                config.cflag("-mthumb");
                config.cxxflag("-mthumb");
            }
            "x86_64" => {
                config.cflag("-march=x86-64");
                config.cxxflag("-march=x86-64");
            }
            "x86" => {
                config.cflag("-march=i686");
                config.cxxflag("-march=i686");
            }
            _ => {}
        }

        // Android-specific CMake configurations
        config.define("GGML_LLAMAFILE", "OFF");

        // Link Android system libraries
        println!("cargo:rustc-link-lib=log");
        println!("cargo:rustc-link-lib=android");
    }

    if matches!(target_os, TargetOs::Linux)
        && target_triple.contains("aarch64")
        && !env::var(format!("CARGO_FEATURE_{}", "native".to_uppercase())).is_ok()
    {
        // If the native feature is not enabled, we take off the native ARM64 support.
        // It is useful in docker environments where the native feature is not enabled.
        config.define("GGML_NATIVE", "OFF");
        config.define("GGML_CPU_ARM_ARCH", "armv8-a");
    }

    if cfg!(feature = "vulkan") {
        config.define("GGML_VULKAN", "ON");
        match target_os {
            TargetOs::Windows(_) => {
                let vulkan_path = env::var("VULKAN_SDK").expect(
                    "Please install Vulkan SDK and ensure that VULKAN_SDK env variable is set",
                );
                let vulkan_lib_path = Path::new(&vulkan_path).join("Lib");
                println!("cargo:rustc-link-search={}", vulkan_lib_path.display());
                println!("cargo:rustc-link-lib=vulkan-1");

                // workaround for this error: "FileTracker : error FTK1011: could not create the new file tracking log file"
                // it has to do with MSBuild FileTracker not respecting the path
                // limit configuration set in the windows registry.
                // I'm not sure why that's a thing, but this makes my builds work.
                // (crates that depend on llama-cpp-rs w/ vulkan easily exceed the default PATH_MAX on windows)
                env::set_var("TrackFileAccess", "false");
                // since we disabled TrackFileAccess, we can now run into problems with parallel
                // access to pdb files. /FS solves this.
                config.cflag("/FS");
                config.cxxflag("/FS");
            }
            TargetOs::Linux => {
                // If we are not using system provided vulkan SDK, add vulkan libs for linking
                if let Ok(vulkan_path) = env::var("VULKAN_SDK") {
                    let vulkan_lib_path = Path::new(&vulkan_path).join("lib");
                    println!("cargo:rustc-link-search={}", vulkan_lib_path.display());
                }
                println!("cargo:rustc-link-lib=vulkan");
            }
            _ => (),
        }
    }

    if cfg!(feature = "cuda") {
        config.define("GGML_CUDA", "ON");

        if cfg!(feature = "cuda-no-vmm") {
            config.define("GGML_CUDA_NO_VMM", "ON");
        }
    }

    // Android doesn't have OpenMP support AFAICT and openmp is a default feature. Do this here
    // rather than modifying the defaults in Cargo.toml just in case someone enables the OpenMP feature
    // and tries to build for Android anyway.
    if cfg!(feature = "openmp") && !matches!(target_os, TargetOs::Android) {
        config.define("GGML_OPENMP", "ON");
    } else {
        config.define("GGML_OPENMP", "OFF");
    }

    // General
    config
        .profile(&profile)
        .very_verbose(std::env::var("CMAKE_VERBOSE").is_ok()) // Not verbose by default
        .always_configure(false);

    let build_dir = config.build();

    // Search paths
    println!("cargo:rustc-link-search={}", out_dir.join("lib").display());
    println!(
        "cargo:rustc-link-search={}",
        out_dir.join("lib64").display()
    );
    println!("cargo:rustc-link-search={}", build_dir.display());

    if cfg!(feature = "cuda") && !build_shared_libs {
        // Re-run build script if CUDA_PATH environment variable changes
        println!("cargo:rerun-if-env-changed=CUDA_PATH");

        // Add CUDA library directories to the linker search path
        for lib_dir in find_cuda_helper::find_cuda_lib_dirs() {
            println!("cargo:rustc-link-search=native={}", lib_dir.display());
        }

        // Platform-specific linking
        if cfg!(target_os = "windows") {
            // ✅ On Windows, use dynamic linking.
            // Static linking is problematic because NVIDIA does not provide culibos.lib,
            // and static CUDA libraries (like cublas_static.lib) are usually not shipped.

            println!("cargo:rustc-link-lib=cudart"); // Links to cudart64_*.dll
            println!("cargo:rustc-link-lib=cublas"); // Links to cublas64_*.dll
            println!("cargo:rustc-link-lib=cublasLt"); // Links to cublasLt64_*.dll

            // Link to CUDA driver API (nvcuda.dll via cuda.lib)
            if !cfg!(feature = "cuda-no-vmm") {
                println!("cargo:rustc-link-lib=cuda");
            }
        } else {
            // ✅ On non-Windows platforms (e.g., Linux), static linking is preferred and supported.
            // Static libraries like cudart_static and cublas_static depend on culibos.

            println!("cargo:rustc-link-lib=static=cudart_static");
            println!("cargo:rustc-link-lib=static=cublas_static");
            println!("cargo:rustc-link-lib=static=cublasLt_static");

            // Link to CUDA driver API (libcuda.so)
            if !cfg!(feature = "cuda-no-vmm") {
                println!("cargo:rustc-link-lib=cuda");
            }

            // culibos is required when statically linking cudart_static
            println!("cargo:rustc-link-lib=static=culibos");
        }
    }

    // Link libraries
    let llama_libs_kind = if build_shared_libs { "dylib" } else { "static" };
    let llama_libs = extract_lib_names(&out_dir, build_shared_libs);
    assert_ne!(llama_libs.len(), 0);

    // Filter out ggml libraries when use-shared-ggml is enabled - they're already linked from ggml-rs
    let llama_libs: Vec<String> = if cfg!(feature = "use-shared-ggml") {
        // Filter out ggml libraries - they're already linked from ggml-rs
        llama_libs
            .into_iter()
            .filter(|lib| !lib.starts_with("ggml"))
            .collect()
    } else {
        // Original code: include all libraries including embedded ggml
        llama_libs
    };

    for lib in llama_libs {
        let link = format!("cargo:rustc-link-lib={}={}", llama_libs_kind, lib);
        debug_log!("LINK {link}",);
        println!("{link}",);
    }

    // OpenMP
    if cfg!(feature = "openmp") && target_triple.contains("gnu") {
        println!("cargo:rustc-link-lib=gomp");
    }

    match target_os {
        TargetOs::Windows(WindowsVariant::Msvc) => {
            println!("cargo:rustc-link-lib=advapi32");
            if cfg!(debug_assertions) {
                println!("cargo:rustc-link-lib=dylib=msvcrtd");
            }
        }
        TargetOs::Linux => {
            println!("cargo:rustc-link-lib=dylib=stdc++");
        }
        TargetOs::Apple(variant) => {
            println!("cargo:rustc-link-lib=framework=Foundation");
            println!("cargo:rustc-link-lib=framework=Metal");
            println!("cargo:rustc-link-lib=framework=MetalKit");
            println!("cargo:rustc-link-lib=framework=Accelerate");
            println!("cargo:rustc-link-lib=c++");

            match variant {
                AppleVariant::MacOS => {
                    // On (older) OSX we need to link against the clang runtime,
                    // which is hidden in some non-default path.
                    //
                    // More details at https://github.com/alexcrichton/curl-rust/issues/279.
                    if let Some(path) = macos_link_search_path() {
                        println!("cargo:rustc-link-lib=clang_rt.osx");
                        println!("cargo:rustc-link-search={}", path);
                    }
                }
                AppleVariant::Other => (),
            }
        }
        _ => (),
    }

    // copy DLLs to target
    if build_shared_libs {
        let mut libs_assets = extract_lib_assets(&out_dir);
        
        // When using shared GGML, filter out embedded GGML DLLs
        // (ggml-rs handles copying its own DLLs)
        if cfg!(feature = "use-shared-ggml") {
            // Determine library base name based on namespace
            let lib_base_name = ggml_namespace.unwrap_or("ggml");
            
            libs_assets.retain(|asset| {
                let filename = asset.file_name().unwrap().to_str().unwrap();
                // Keep llama.dll and other non-GGML DLLs
                // Filter out ggml*.dll (ggml.dll, ggml-base.dll, ggml-cpu.dll, etc.)
                // Also filter out namespace-aware names (ggml_llama*.dll, ggml_whisper*.dll)
                !filename.starts_with("ggml")
            });
            
            // Copy ALL namespace-specific GGML libraries from ggml-rs
            // This ensures all 4 DLLs are copied: base, base-base, base-cpu, base-cuda (if enabled)
            if let Some(ref lib_dir) = ggml_lib_dir {
                if lib_dir.exists() {
                    let shared_lib_pattern = if cfg!(windows) {
                        "*.dll"
                    } else if cfg!(target_os = "macos") {
                        "*.dylib"
                    } else {
                        "*.so"
                    };
                    
                    let pattern = lib_dir.join(shared_lib_pattern);
                    debug_log!("Looking for namespace-specific GGML libraries in: {}", pattern.display());
                    
                    // List of libraries to copy based on namespace
                    let base_lib = lib_base_name.to_string();
                    let base_base_lib = format!("{}-base", lib_base_name);
                    let base_cpu_lib = format!("{}-cpu", lib_base_name);
                    let libraries_to_copy = vec![
                        &base_lib,      // e.g., ggml_llama
                        &base_base_lib, // e.g., ggml_llama-base
                        &base_cpu_lib,  // e.g., ggml_llama-cpu
                    ];
                    
                    // Add feature-specific libraries if enabled
                    let mut feature_libs = Vec::new();
                    if cfg!(feature = "cuda") {
                        feature_libs.push(format!("{}-cuda", lib_base_name));
                    }
                    if cfg!(feature = "vulkan") {
                        feature_libs.push(format!("{}-vulkan", lib_base_name));
                    }
                    if cfg!(feature = "metal") {
                        feature_libs.push(format!("{}-metal", lib_base_name));
                    }
                    
                    let mut copied_count = 0;
                    for entry in glob(pattern.to_str().unwrap()).unwrap() {
                        match entry {
                            Ok(path) => {
                                let filename = path.file_name().unwrap().to_str().unwrap();
                                
                                // Check if this is a namespace-specific runtime library (DLL/dylib/so) we need to copy
                                // Note: We only copy runtime libraries, not linking libraries (.lib files)
                                let should_copy = libraries_to_copy.iter().any(|lib_name| {
                                    if cfg!(windows) {
                                        // Only copy .dll files, not .lib files (those are for linking)
                                        filename == format!("{}.dll", lib_name)
                                    } else if cfg!(target_os = "macos") {
                                        filename == format!("lib{}.dylib", lib_name)
                                    } else {
                                        filename == format!("lib{}.so", lib_name)
                                    }
                                }) || feature_libs.iter().any(|lib_name| {
                                    if cfg!(windows) {
                                        // Only copy .dll files, not .lib files (those are for linking)
                                        filename == format!("{}.dll", lib_name)
                                    } else if cfg!(target_os = "macos") {
                                        filename == format!("lib{}.dylib", lib_name)
                                    } else {
                                        filename == format!("lib{}.so", lib_name)
                                    }
                                });
                                
                                if should_copy {
                                    println!("cargo:warning=[GGML] Copying namespace-specific library: {}", filename);
                                    debug_log!("Found namespace-specific GGML library: {}", path.display());
                                    libs_assets.push(path);
                                    copied_count += 1;
                                }
                            }
                            Err(e) => debug_log!("Error globbing for GGML libraries: {}", e),
                        }
                    }
                    
                    println!("cargo:warning=[GGML] Copied {} namespace-specific GGML libraries", copied_count);
                } else {
                    eprintln!("cargo:warning=[GGML] Library directory does not exist: {}", lib_dir.display());
                }
            } else {
                eprintln!("cargo:warning=[GGML] Library directory not found. Make sure ggml-rs is built with namespace-llama feature.");
            }
        }
        
        for asset in libs_assets {
            let asset_clone = asset.clone();
            let filename = asset_clone.file_name().unwrap();
            let filename = filename.to_str().unwrap();
            let dst = target_dir.join(filename);
            debug_log!("HARD LINK {} TO {}", asset.display(), dst.display());
            if !dst.exists() {
                std::fs::hard_link(asset.clone(), dst).unwrap();
            }

            // Copy DLLs to examples as well
            if target_dir.join("examples").exists() {
                let dst = target_dir.join("examples").join(filename);
                debug_log!("HARD LINK {} TO {}", asset.display(), dst.display());
                if !dst.exists() {
                    std::fs::hard_link(asset.clone(), dst).unwrap();
                }
            }

            // Copy DLLs to target/profile/deps as well for tests
            let dst = target_dir.join("deps").join(filename);
            debug_log!("HARD LINK {} TO {}", asset.display(), dst.display());
            if !dst.exists() {
                std::fs::hard_link(asset.clone(), dst).unwrap();
            }
        }
    }
    
    // Note: When use-shared-ggml is enabled, base GGML DLLs are handled by ggml-rs.
    // Feature-specific libraries (cuda, vulkan, metal) are copied above.
}

