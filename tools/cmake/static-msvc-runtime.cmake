# Loaded by CMake builds of native dependencies (currently only Opus, through
# opusic-sys) via CMAKE_TOOLCHAIN_FILE in .cargo/config.toml.
#
# Rust links the MSVC C runtime statically (+crt-static), and every C library in
# the executable must use the same runtime. Opus selects its runtime itself and
# defaults to the DLL; opusic-sys offers no way to change that, so set the
# switch here. It has no effect on non-MSVC toolchains.
set(OPUS_STATIC_RUNTIME ON CACHE BOOL "Link the static MSVC runtime" FORCE)
