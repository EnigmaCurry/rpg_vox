# CMake toolchain for C deps (libopus) built by cargo-ndk: pins the ABI
# and API level, which the NDK toolchain otherwise defaults to armv7.
set(ANDROID_ABI arm64-v8a)
set(ANDROID_PLATFORM android-26)
include($ENV{ANDROID_NDK_HOME}/build/cmake/android.toolchain.cmake)
