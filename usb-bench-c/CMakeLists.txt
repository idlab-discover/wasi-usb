cmake_minimum_required(VERSION 3.20)
project(usb-bench-c C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)

# ---------------------------------------------------------------------------
# Repository root (parent of this directory)
# ---------------------------------------------------------------------------
get_filename_component(REPO_ROOT "${CMAKE_SOURCE_DIR}/.." ABSOLUTE)
set(LIBUSB_DIR "${REPO_ROOT}/libusb")

# ---------------------------------------------------------------------------
# Helper macro: configure one benchmark executable for the current target
#   bench_target(<name>)
#   Sources: src/csv.c  src/<name>.c
# ---------------------------------------------------------------------------
macro(bench_target _name)
    add_executable(${_name} src/csv.c src/${_name}.c)
    target_include_directories(${_name} PRIVATE src)
    target_compile_options(${_name} PRIVATE -Wall -Wextra)

    if(CMAKE_SYSTEM_NAME STREQUAL "WASI")
        # ── C2: wasi-libusb ─────────────────────────────────────────────────
        target_include_directories(${_name} PRIVATE "${LIBUSB_DIR}/libusb")
        target_link_libraries(${_name} PRIVATE
            "${LIBUSB_DIR}/libusb-wasi.a"
            "${CMAKE_SOURCE_DIR}/bindings/guest_component_type.o"
        )
        target_compile_definitions(${_name} PRIVATE BENCH_CONDITION="wasi-libusb")
        set_target_properties(${_name} PROPERTIES SUFFIX ".wasm")
    else()
        # ── C1: native libusb ────────────────────────────────────────────────
        target_include_directories(${_name} PRIVATE
            ${LIBUSB_INCLUDE_DIRS}
            "${LIBUSB_DIR}/libusb"
        )
        target_link_libraries(${_name} PRIVATE ${LIBUSB_LIBRARIES})
        target_link_directories(${_name} PRIVATE ${LIBUSB_LIBRARY_DIRS})
        target_compile_definitions(${_name} PRIVATE BENCH_CONDITION="native-libusb")
    endif()

    install(TARGETS ${_name} DESTINATION "${REPO_ROOT}/bench/bin")
endmacro()

# ---------------------------------------------------------------------------
# Target-specific setup (once, shared by all bench_target() calls)
# ---------------------------------------------------------------------------
if(CMAKE_SYSTEM_NAME STREQUAL "WASI")
    message(STATUS "Configuring for WASI (C2 — wasi-libusb)")
    message(STATUS "  libusb header : ${LIBUSB_DIR}/libusb")
    message(STATUS "  libusb archive: ${LIBUSB_DIR}/libusb-wasi.a")

    if(NOT EXISTS "${LIBUSB_DIR}/libusb-wasi.a")
        message(FATAL_ERROR
            "libusb-wasi.a not found at ${LIBUSB_DIR}/libusb-wasi.a\n"
            "Build it following ${LIBUSB_DIR}/BUILDING_WASI.md")
    endif()

    if(NOT EXISTS "${CMAKE_SOURCE_DIR}/bindings/guest_component_type.o")
        message(FATAL_ERROR
            "bindings/guest_component_type.o not found.\n"
            "Generate it with:\n"
            "  wit-bindgen c --world guest --out-dir bindings ../wit\n"
            "  wasm32-wasip2-clang -c bindings/guest.c "
            "-o bindings/guest_component_type.o")
    endif()
else()
    message(STATUS "Configuring for native host (C1 — native-libusb)")
    find_package(PkgConfig REQUIRED)
    pkg_check_modules(LIBUSB REQUIRED libusb-1.0)
    message(STATUS "  libusb include : ${LIBUSB_INCLUDE_DIRS}")
    message(STATUS "  libusb libs    : ${LIBUSB_LIBRARIES}")
endif()

# ---------------------------------------------------------------------------
# Benchmarks
# ---------------------------------------------------------------------------
bench_target(w_bulk)   # W-bulk : SCSI READ(10) on USB mass-storage
bench_target(w_ctrl)   # W-ctrl : GET_DESCRIPTOR on Arduino Nano 33 BLE
bench_target(w_int)    # W-int  : interrupt poll on PS5 DualSense
bench_target(w_iso)    # W-iso  : isochronous UVC stream on Logitech Brio

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
message(STATUS "")
message(STATUS "  Targets : w_bulk  w_ctrl  w_int  w_iso")
message(STATUS "  Platform: ${CMAKE_SYSTEM_NAME}")
message(STATUS "  Suffix  : ${CMAKE_EXECUTABLE_SUFFIX}")
message(STATUS "")
message(STATUS "  Build:  cmake --build .")
