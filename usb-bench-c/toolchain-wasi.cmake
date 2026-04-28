# CMake toolchain file for wasm32-wasip2 (WASI Preview 2) using WASI SDK.
#
# Usage:
#   cmake -B build-wasi \
#         -DCMAKE_TOOLCHAIN_FILE=../toolchain-wasi.cmake \
#         ..
#
# Optional: override the WASI SDK path (default: /opt/wasi-sdk)
#   cmake -B build-wasi \
#         -DCMAKE_TOOLCHAIN_FILE=../toolchain-wasi.cmake \
#         -DWASI_SDK_PATH=/path/to/wasi-sdk \
#         ..

# ---------------------------------------------------------------------------
# WASI SDK path — override with -DWASI_SDK_PATH=<path> if needed
# ---------------------------------------------------------------------------
if(NOT DEFINED WASI_SDK_PATH)
    set(WASI_SDK_PATH "/opt/wasi-sdk" CACHE PATH "Path to the WASI SDK installation")
endif()

if(NOT EXISTS "${WASI_SDK_PATH}/bin/wasm32-wasip2-clang")
    message(FATAL_ERROR
        "WASI SDK not found at '${WASI_SDK_PATH}'.\n"
        "Install it from https://github.com/WebAssembly/wasi-sdk/releases\n"
        "or set -DWASI_SDK_PATH=/path/to/wasi-sdk.")
endif()

# ---------------------------------------------------------------------------
# Target system
# ---------------------------------------------------------------------------
set(CMAKE_SYSTEM_NAME  WASI)
set(CMAKE_SYSTEM_VERSION 1)
set(CMAKE_SYSTEM_PROCESSOR wasm32)

# ---------------------------------------------------------------------------
# Compilers
# ---------------------------------------------------------------------------
set(CMAKE_C_COMPILER   "${WASI_SDK_PATH}/bin/wasm32-wasip2-clang")
set(CMAKE_CXX_COMPILER "${WASI_SDK_PATH}/bin/wasm32-wasip2-clang++")
set(CMAKE_AR           "${WASI_SDK_PATH}/bin/llvm-ar")
set(CMAKE_RANLIB       "${WASI_SDK_PATH}/bin/llvm-ranlib")

# ---------------------------------------------------------------------------
# Sysroot
# ---------------------------------------------------------------------------
set(WASI_SYSROOT "${WASI_SDK_PATH}/share/wasi-sysroot")
set(CMAKE_SYSROOT "${WASI_SYSROOT}")

set(CMAKE_C_FLAGS_INIT   "--sysroot=${WASI_SYSROOT}")
set(CMAKE_CXX_FLAGS_INIT "--sysroot=${WASI_SYSROOT}")
set(CMAKE_EXE_LINKER_FLAGS_INIT "--sysroot=${WASI_SYSROOT}")

# ---------------------------------------------------------------------------
# Search paths: find only WASI libraries/includes, never host
# ---------------------------------------------------------------------------
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)

# ---------------------------------------------------------------------------
# Prevent CMake from adding host-platform RPATH flags
# ---------------------------------------------------------------------------
set(CMAKE_SKIP_RPATH TRUE)
